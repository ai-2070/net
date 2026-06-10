# Performance Audit — Hot Path Survey (2026-06-09)

Source: code inspection of the mesh packet path (`src/adapter/net/`), the in-process
bus ingest path (`src/bus.rs`, `src/shard/`, `src/timestamp.rs`), and a sweep of the
existing perf plan docs to establish what is already identified, shipped, or gated.
Widened same-day in two rounds: §8–§11 cover the nRPC dispatch layer above the
transport (`mesh_rpc.rs` + `cortex/rpc.rs`), the FFI/bindings surface (the path SDK
consumers actually hit), and the RedEX/CortEX/consumer-drain paths; §12–§14 cover
the remainder — behavior/ modules, the Dataforts blob path, and the control-plane
surfaces — all of which came back clean, completing coverage of the crate.
Numbers from `BENCHMARKS.md` (2026-04-27, M1 Max + i9-14900K) and the prior audits are
used as the baseline; per-item costs below marked *est.* are reasoned from the code,
not re-measured.

> **Status: findings only (2026-06-09).** No fixes applied. This is a survey answering
> "is there meaningful hot-path headroom left, and where" — the answer is *yes, but
> almost entirely on the mesh/nRPC wire path, not the bus core*. Several of the largest
> levers are already designed and sitting at decision gates (see §1, §2); this doc adds
> two genuinely new findings (§4, §5) and consolidates the rest in one place.

The headline conclusion, consistent with `PERF_AUDIT_2026_05_19_NRPC.md` and
`docs/plans/NRPC_FLAMEGRAPH.md`: **the system is syscall- and wakeup-bound, not
compute-bound.** Flamegraph attribution: 51% wake/scheduling, 22% transport syscalls,
~5% AEAD. Compute-side micro-optimization of the bus core is done (§7); the remaining
headroom is in collapsing syscalls, eliminating wakeups, and one unconditional RX-path
allocation.

Recommended order of attack is at the bottom.

---

## 1. (Highest leverage) Recv-loop batching is built but parked — run the c128 gate, land the channel-hop gap-fix first

**Status of the work.** `batched-ingress` (Linux `recvmmsg`, `MAX_BATCH_SIZE = 64`) is
implemented through Stage 5 of `docs/plans/NRPC_RECV_LOOP_BATCHING_PLAN.md` but is
**default-off** behind both a Cargo feature and a runtime flag
(`MeshNodeConfig::batched_ingress`). Flipping the default is parked pending the c128
measurement.

- Default RX path: one `recvfrom` per packet — `PacketReceiver::recv()` →
  `socket.recv_buf_from()` (`src/adapter/net/transport.rs:396-401`).
- Batched RX path: `recvmmsg` wrapper (`src/adapter/net/linux.rs:316,422,626-647`),
  `BatchedPacketReceiver` dedicated OS thread + bounded mpsc
  (`src/adapter/net/transport.rs:303`), wired into the adapter at
  `src/adapter/net/mod.rs:817`. Gating at `src/adapter/net/mesh.rs:3287-3304`.

**Why this is the top item.** `NRPC_QPS_CONCURRENCY_SCALING_PLAN.md` (Phase 0,
diagnosed) shows the single shared recv loop is the structural concurrency ceiling:
c1→c16 scales only ~4×, worker-count sweeps don't move it, and recv syscalls + wakeups
account for 22% of CPU. The plan's own conclusion was "no safe in-scope lever" with
batched recv named as one of the two real levers.

**Gap-fix to land *before* measuring** (plan gap-fix #1, ~40 LoC): the batched receiver
currently forwards packets **one at a time** through the mpsc channel, so a 64-packet
`recvmmsg` batch degrades into 64 channel sends — the syscall collapse survives but the
wakeup collapse (the larger cost, per the flamegraph) is thrown away. Change the channel
payload to `Vec<(Bytes, SocketAddr)>` (one send per batch). Measuring without this
understates the design and could fail the gate for the wrong reason.

**Known risk the gate must check** (plan gap-fix #2 / measurement criteria): the
cross-thread hop taxes *every* inbound packet, including lone unary nRPC at c1 — verify
unary p50/p99 doesn't regress, and consider replacing the 1 ms busy-poll idle with a
kernel-blocking poll before default-on.

---

## 2. Ack-piggyback wire change — the only lever on the unary QPS ceiling

Per `NRPC_FLAMEGRAPH.md`, grants are the sole ACK and cost 4–6 spawns per round trip
(1–3 µs each, 5–15 µs total) on a path that is wake-latency bound. Skipping grants on
unary was evaluated and correctly rejected as unsafe (grants are the retransmit
backstop). The safe version — piggybacking acks on response traffic — is
design-complete in `NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md` but unimplemented.

This is the path from the ~70K QPS plateau toward the 150–200K target
(`PERF_AUDIT_2026_05_19_NRPC.md`); nothing smaller in scope reaches it. It is a wire
format change, so it carries the usual cross-binding compat work (golden vectors,
`bindings/{node,python}` compat tests).

---

## 3. Crypto SIMD opt-in is being left on the table in deployments

Carried forward from `PERF_AUDIT_2026_06_08_BENCHMARK_WINS.md` §1 (documented opt-in,
deliberately not enforced in committed config): default builds compile
ChaCha20-Poly1305 to the portable backend, a fixed ~1.0–1.1 µs per packet independent
of payload — the bulk of the ~5 µs transport+crypto floor. `RUSTFLAGS="-C
target-feature=+avx2"` recovers 5–10× on the fixed cost (64B encrypt: 1.13 µs →
~100–200 ns).

The committed-config decision stands; the open question is **deployment-side**: the
published artifacts (Python wheel, Node prebuilds, release binaries) currently ship the
portable path. Worth deciding per-artifact whether to build with arch feature flags (or
fat/multiarch dispatch) rather than leaving the 5–10× as a footnote only source builds
can claim. Cipher caching itself is correct — instance per session, key schedule
reused, stack nonce from a shared counter (`src/adapter/net/crypto.rs:596-649`,
`pool.rs:679-705`, `session.rs:129`); the fixed cost is purely the backend selection.

---

## 4. NEW: per-event `String` allocation on the inbound hot path

The **only unconditional heap allocation** on the RX fast path
(`src/adapter/net/mesh.rs:5068-5070`):

```rust
let mut event_id = String::with_capacity(24);
let _ = write!(event_id, "{}:{}", seq, i);
queue.push(StoredEvent::new(event_id, event_data, seq, shard_id));
```

One `String` alloc + format per inbound event, every event, on a path where decrypt is
in-place (no alloc when `Bytes` refcount == 1) and `EventFrame::read_events()` slices
without copying. *Est.* ~100–150 ns per event; packets often carry multiple events, so
this stacks per packet. At high pps it is also pure allocator pressure.

**Fix direction** (no protocol change): `StoredEvent::event_id` is `seq:index` by
construction — store it as a `(u64, u32)` pair (or inline-array small string) and
format lazily only where something actually renders it. Touches `StoredEvent` and its
consumers; wire format unaffected.

---

## 5. NEW: unary sends bypass the shipped sendmmsg batching; routed-path copies

- **Send batching only covers scheduled streams.** Phase 1 of
  `NRPC_SEND_LOOP_BATCHING_PLAN.md` shipped (62× syscall collapse on concurrent
  scheduled streams via per-destination `send_batch` / `sendmmsg`,
  `src/adapter/net/transport.rs:710-715`, `linux.rs:169-220`). But unary RPC sends go
  directly out via `socket.send_to` (`src/adapter/net/mesh.rs:8342`), one syscall per
  packet, and the event publish path flushes `current_batch` to a per-packet
  `send_to` as well (`mesh.rs:5531-5558`). Encryption is amortized across the batch;
  the syscalls are not. Expect this to matter only after §1/§2 move the bottleneck.
- **Routed/forwarded packets allocate per hop**: fresh `BytesMut` + two memcpys to
  splice the forward header (`mesh.rs:3561`, similar at `3627`, `3784`, `5627`,
  `5655`). Cold relative to the direct path; acceptable trade-off today, listed so
  it's on record if relay-heavy topologies become a profile.
- **Retransmit path deep-copies** (`mesh.rs:4286`, `Bytes::copy_from_slice` per rebuilt
  packet) and sends sequentially inside a spawn (`mesh.rs:4292-4295`) — loss-path only,
  fine in steady state.

---

## 6. Smaller already-scoped nRPC items (from `NRPC_FLAMEGRAPH.md`, not yet landed)

T1.1 (grant coalescing) and T1.2 (response via `publish_to_peer`) landed. Still open:

| item | est. win | note |
|---|---|---|
| T3.4 drop `catch_unwind` on dispatch | 100–300 ns/call | cheap, low-risk |
| T2.2 `encode_into(BytesMut)` | 200–400 ns @ 1 KiB | kills an encode-side copy |
| T2.1 drop fold `Mutex` | helps c128 ceiling | medium risk, re-bench after §1 |

Also still pending from `PERF_AUDIT_2026_05_28_CAPABILITY.md`: fix #3 (compact codec
for capability serialize — wire-format gate) and #4 (`OnceCell` projection cache).

---

## 7. Bus core: done — verified, leave it alone

The ingest path was audited end-to-end and is as tight as its design allows. For the
record, the per-event cost structure on the `ingest_raw` fast path (~18 ns measured):

| component | cost (est.) | verdict |
|---|---|---|
| shutdown guard, `fetch_add`/`load` SeqCst pair + RAII decrement (`bus.rs:909-916`) | ~10 ns | only candidate, see below |
| shard select, Lemire multiply-shift (`shard/mod.rs:473-501`) | 1–2 ns | optimal |
| timestamp, TSC + monotonic CAS, per-shard generator (`timestamp.rs:100-152`) | 6–12 ns | correct & contention-free; the in-loop TSC re-read is deliberate (drift fix) |
| ring buffer push, 2 atomics, `CachePadded` head/tail (`shard/ring_buffer.rs:233-261`) | 2–3 ns | optimal; `pop_batch_into` amortizes the consumer side to ~0 |
| `Bytes` refcount bump (`event.rs:238-249`) | 2–5 ns | inherent to zero-copy |
| stats counter, Relaxed `fetch_add` (`shard/mod.rs:154-156`) | ~1 ns | negligible |

Two footnotes, neither urgent:

- **Shutdown-guard ordering** (`bus.rs:909-916`): if the "no producer mid-push when
  drain workers sweep" contract survives an Acquire/Release downgrade of the SeqCst
  pair, ~5–10 ns/event comes back — but SeqCst is plausibly load-bearing here; a loom
  model (`tests/loom_models.rs` infrastructure already exists) should decide, not
  intuition.
- **Per-push metrics when a collector is armed** (`shard/mod.rs:157-159`):
  `Instant::now()` + `elapsed()` + two records per push, *est.* 50–200 ns — fine as an
  opt-in instrument, just never arm it on a production ingest path; consider sampling
  (1-in-N) if it's ever needed live.

Also verified clean on the mesh RX path, listed so nobody re-audits them: session
lookup is 2 uncontended DashMap gets + a Relaxed cached-node-id load
(`mesh.rs:3605-3617`, `session.rs:165-170`); the replay-window mutex is
correctness-required and already fused with the admit counter; per-packet tracing is
error-path only; task spawns are confined to control/recovery paths (NACK, NAT,
migration — `mesh.rs:4120,4292,4567`) and don't fire in steady state.

---

## 8. WIDENED: nRPC dispatch layer — flamegraph items verified, two NEW per-call costs

The dispatch layer (`mesh_rpc.rs` + `cortex/rpc.rs`) was traced end-to-end for one
unary RPC on both sides. First, current state of the known `NRPC_FLAMEGRAPH.md` items
— **two of them are in worse shape than their plan status suggests**:

| item | plan status | actual state (2026-06-09) |
|---|---|---|
| T1.1 grants on unary | "landed (drainer)" | drainer landed, but grants still fire **unconditionally on every accepted packet** (`mesh.rs:4771` → `session.rs:1077-1106` always grants when `window_bytes != 0`, default 65536) — 2 grant wakeups per unary RT remain; full fix is the ack-piggyback plan (§2) |
| T1.2 response via `publish_to_peer` | "landed" | **partially landed**: the origin cache + `publish_response_to_caller` exist (`mesh_rpc.rs:1561-1587`), but the emit closure still wraps every response in `tokio::spawn` (`mesh_rpc.rs:1798`) — see new finding 8a |
| T2.2 `encode_into` | open | confirmed open: no `encode_into` variant exists; `req.encode()` allocates a `Vec` then `extend_from_slice`s into the caller's buf — double copy (`mesh_rpc.rs:3102-3104`) |
| T3.4 `catch_unwind` | open | confirmed open, always-on, 4 sites (`cortex/rpc.rs:1613,2281,2792,3217`) |
| T2.1 fold/in-flight Mutex | open | confirmed open; `in_flight` Mutex is locked **twice** on the request path (dup-check `cortex/rpc.rs:1543`, insert `:1563`) + once more in the spawned task (`:1681`) |

**New findings (not in the flamegraph catalog):**

- **8a. One `tokio::spawn` per response** (`mesh_rpc.rs:1798-1833`): the emit closure
  spawns a task for every response publish. *Est.* 1–2 µs scheduling per RT, and it is
  one of the 4–6 spawns the flamegraph counted. The T1.2 cache hit makes the publish
  itself cheap (`publish_to_peer`), so the spawn is now the dominant cost of the
  response leg — inline the publish into the handler task (it's already spawned) or
  feed a drainer like T1.1 did.
- **8b. Reply-channel string per response** (`mesh_rpc.rs:1799-1800`):
  `format!("{service}.replies.{caller_origin:016x}")` + `ChannelName::new()` — two
  heap allocs per response that are deterministic from `(service, caller_origin)` and
  cacheable alongside the T1.2 origin cache. *Est.* 50–100 ns.
- **8c. Per-call alloc/lock census** (for the record): 9–12 heap allocations and ~6
  lock/DashMap touches per unary RT across both sides (header Vec, `service.to_string()`
  `mesh_rpc.rs:3095`, encode bufs ×2, decode service String, reply-channel ×2, pending
  insert, route cache, cancel registry `:3191`, in-flight ×3, origin cache).
- **8d. Metrics are always-on**, *est.* ~400–500 ns per RT: client + server guards bump
  in-flight/outcome atomics and each walk an **11-bucket latency histogram loop**
  (`mesh_rpc_metrics.rs:136-175,679-720`, `cortex/rpc.rs:1591-1625`). Atomics-only (no
  allocs) so it hides from heap profiles; visible at µbench scale. A
  one-branch `metrics_enabled` gate would reclaim it where wanted.
- **8e. Wakeup count confirmed**: 4–5 wakeup events per unary RT as the code stands
  (grant drainer notify, handler spawn, response spawn, client oneshot) — matches the
  51% futex-wait attribution. §1 + §2 + 8a together are what move this.

---

## 9. WIDENED: FFI / bindings layer — clean; the gap is defaults, not code

All published benchmarks measure the Rust core; SDK consumers cross this layer. Verdict:
**the bindings practice zero-copy discipline and are not the bottleneck** — worth
recording so nobody re-audits them:

- C FFI: handle validation is pointer-alignment only (no registry lock); per-call cost
  is an atomic guard pair (~20 ns, `ffi/handle_guard.rs:88-114`); `net_ingest_raw_batch`
  crosses the boundary once for N events (`ffi/mod.rs:886-983`).
- Node/NAPI: `push()`/`push_batch()` are zero-copy Buffer paths, ~50–100 ns/event
  (`bindings/node/src/lib.rs:382-431`); `ingest_raw_sync` pays a JSON parse (~500 ns) —
  control-plane only.
- Python: `ingest_raw_batch` ~100–200 ns/event (`bindings/python/src/lib.rs:646-656`);
  the dict-taking `ingest()` costs 0.5–2 µs (GIL + `json.dumps`, `:633-637`).
- Go: no Rust-side binding cost, but the cgo crossing itself is ~0.5–3 µs per call —
  batch API is *essential* for Go consumers, not an optimization.

**Actionable:** this is a docs/defaults gap, not a code gap. The batch APIs exist and
are correctly annotated as "most efficient"; SDK examples and quickstarts should make
them the *default* shape (especially Go and Python), and the dict/JSON-string
convenience paths should carry a "control-plane only" note. The marketplace/L0
integration goes through these SDKs, so the per-call tier table above is the real
consumer-facing perf story.

---

## 10. WIDENED: RedEX append + watcher wake path

Phases 1–4 of `REDEX_DISK_THROUGHPUT_PLAN.md` verified present in the code (coalesced
dat/idx/ts writes `redex/disk.rs:1027-1127`, atomic fsync signaling `:600-632`). Two
items:

- **Per-event, per-subscriber watcher sends** (`redex/file.rs:1561-1580`): delivery
  does `try_send(Ok(event.clone()))` per watcher per event — at 100K ev/s × 5 watchers
  that is 500K channel wakes/s, and it is where the "RedEX wake: 5–10 µs" line in
  `PERF_AUDIT_2026_05_19_NRPC.md` lives. The v1 watcher model is live-tail by design;
  batched delivery is a v2 architecture item. Listed as the known ceiling, not a bug.
- **`bench_append_batch_disk` has still never been run** — phases 1–4 claim
  multi-× wins with no captured before/after. Cheap to close; do it with the next
  bench sweep.

CortEX fold dispatch: decode path is zero-copy (`Bytes::slice`, postcard), context
construction allocation-free (`cortex/rpc.rs:1495,1598-1604`); the only hot-path issue
is the in-flight Mutex already tracked as T2.1 (§8 table).

---

## 11. WIDENED: consumer drain/delivery — clean

Audited the other side of the bus (drain → subscriber): `pop_batch_into` amortization,
adaptive batcher velocity sampling (dual-bounded deque, recalc every 10 ms, not
per-event — `shard/batch.rs:81-122`), merge-side filter eval on pre-compiled paths
(`consumer/merge.rs:721`) and the O(n log n) cross-shard ordering sort (`:758-763`)
are all the intended per-event/per-poll costs of the features they implement. No
findings; do not re-audit.

---

## 12. WIDENED (round 3): behavior/ modules — prior fixes verified, no new findings

All fixes from `PERF_AUDIT_2026_05_28_CAPABILITY.md` and `PERF_AUDIT_2026_06_08
_BENCHMARK_WINS.md` verified present in the code (sorted-tag `sort_unstable` +
`sort_by_cached_key` `capability.rs:1856,1865`, tag-direct filter fast paths `:2369`,
axis_key alloc removal `:1349,1372`, AtomicUsize counters across swarm/metadata/
proximity/route). The two deliberately-pending items stand: compact codec (fix #3,
blocked on `#[serde(skip_serializing_if)]` signed-bytes compat, documented at
`capability.rs:2061`) and the OnceCell projection cache (fix #4, deferred pending
evidence of repeated `views()` on the same set).

The previously-unaudited surfaces are clean, classified by where they run:

- **Fold dispatch + apply** (hot, per-announce): RwLock read + O(1) HashMap lookup
  (`fold/dispatch.rs:212-222`), O(1) merge decide, postcard encode/decode ~100–200 ns
  per envelope — intrinsic wire cost. Ed25519 verify (~50 µs) dominates every inbound
  envelope; the double postcard encode in sign/verify (`fold/wire.rs:243-346`) is
  <0.3% of that. Nothing to do without a wire-format change.
- **Safety envelope** (hot, per-check): kill-switch bool + rate-limit atomics +
  policy loop, ~10–50 ns (`safety.rs:1125-1151`); regex compilation happens at
  envelope-update time, never per-check (`safety.rs:1091,1119`). Clean.
- **Group/subnet tag parse** (warm, per-announce): hex decode ~50–100 ns. Clean.
- **Metadata upsert** (hot): one `&'static str → String` index-key alloc per upsert
  (`metadata.rs:1344`), ~20–50 ns, intrinsic to owned-key indexes. Not worth chasing.

No new regressions. **behavior/ is closed.**

---

## 13. WIDENED (round 3): Dataforts blob path — clean; one opt-in lock noted

The throughput-critical paths verified, with several previously-landed perf fixes
confirmed in code:

- **Put**: CDC chunking is zero-copy (`BytesMut::split_to(..).freeze()`,
  `dataforts/blob/cdc.rs:379`) with the adversarial-input rescan amortizer
  (`cdc.rs:282-348`); per-chunk BLAKE3 is stateless/SIMD; stream accumulation drains
  via `mem::replace`, no per-chunk copy (`blob/mesh.rs:1274-1298`). Clean.
- **Get**: manifest fetch is 16-way concurrent (`buffered(16)`, `mesh.rs:3255-3297`),
  reassembly preallocates and pays exactly one memcpy per chunk (optimal for joining
  discontiguous buffers, `mesh.rs:3299-3311`), range fetches return zero-copy
  `Bytes::slice` (`mesh.rs:3410-3470`), verification is single-pass
  (`blob/dispatch.rs:124-206`). Clean.
- **Erasure coding**: stripe-level, runs only on RS-encoded put / repair / future
  auto-repair — correctly off the healthy-blob hot path. GC and pinning are
  background-only. Clean.
- **Tree node cache**: O(1) after the `lru`-crate fix (`blob_tree_cache.rs:66-200`).

One finding, LOW severity: **per-fetch heat-registry mutex** (`mesh.rs:764-771`) —
when data-gravity heat is enabled (opt-in via `with_blob_heat`), every fetch takes
the registry lock to bump 1–128 counters. The lock never spans I/O, so this is
CPU-contention-only under very high fan-out reads; if it ever profiles, batch the
bumps or shard the registry. Informational: the `net-blob` CLI `put` full-buffers the
input file (`bin/net-blob.rs:353-387`) — fine for an operator tool, but worth a doc
note pointing bulk producers at `store_stream_tree`. **Dataforts is closed.**

---

## 14. WIDENED (round 3): control-plane surfaces — all correctly cold; benchmark "alarms" root-caused

The key question — does any gated/control-plane surface leak work onto the hot path —
answers **no** across the board:

- **FailureDetector** — the alarming benchmark rows (`check_all` 342 ms, `stats`
  80–100 ms in BENCHMARKS.md) are **benchmark-fixture artifacts of an O(nodes) scan
  reported per-element**, not hot-path costs: `check_all` runs once per
  `heartbeat_interval` (default 5 s, driven from `mesh.rs:5446`) and costs ~204 µs at
  5,000 nodes (the scaling table in BENCHMARKS.md confirms) — ~40 µs/s amortized.
  Callbacks fire after the iteration lock is released (`failure.rs:271-275`). `stats`
  is observability-only by documented design (`failure.rs:358-362`). Per-heartbeat
  costs on the actual hot path are 14–242 ns. Consider annotating those two rows in
  BENCHMARKS.md so nobody else flags them.
- **MeshOS**: reconcile is tick-driven (~250 ms, `behavior/meshos/event_loop.rs:
  803-940`), drains a bounded 32-event batch per tick, and does NOT subscribe to the
  full bus stream — discrete `MeshOsEvent`s fan in from subsystems. Clean.
- **Aggregator-daemon**: interval-driven (`behavior/aggregator/daemon.rs:197-224`),
  O(fold entries) per tick, no live subscription. Clean.
- **NetDB/MeshDB**: query-on-demand over snapshots; no live subscriber, no per-event
  work when idle. Clean.
- **Deck**: watch-driven snapshot reads; no resident stats polling against the
  expensive aggregate paths. Clean.

**Control-plane is closed. With §8–§14, every surface in the net crate has now been
audited or has a recorded clean verdict; the survey is complete.**

---

## 15. CORRECTNESS (found while baselining the QPS bench): a node expires its own capability entry and denies its own services — ✅ FIXED

Not a perf item, but surfaced by this work and worth recording. While capturing a
`nrpc_qps` baseline to measure §8a, the **c128** bar deterministically panicked with
`CapabilityDenied`. Root cause (instrumented, conclusive):

1. At c128 the transport saturates; `call_json_direct_retrying` retried `Transport`
   backpressure with no deadline, so the bar **livelocked** — it ran for **>300 s**
   instead of ~8 s, making ~no progress. (Same livelock that hung the 16 KiB bar.)
2. Capability fold entries carry a TTL (default 300 s) and the sweeper reaps them on
   expiry. **Nothing periodically re-announced the node's own entry** — no background
   loop did, and `serve_rpc`'s announce is one-time. So ~300 s in, the self-entry
   expired (`lived_ms=300433`, instrumented) and was swept (`total_nodes → 0`).
3. The callee-side cap-auth gate (`may_execute(self, …)`, `mesh_rpc.rs`) then found no
   self-entry → `CapabilityDenied` on every inbound call. Peers also expire the node's
   announcement after one TTL, so it stops being discoverable too.

So **any node serving RPC continuously past one TTL (≈5 min) without re-announcing
would start rejecting all inbound calls AND drop off discovery** — a self-inflicted
outage, masked until now because every test/bench runs far under 300 s (only the c128
livelock ran long enough to trip it).

**Fixes (committed):**
- **Periodic re-announce** — `spawn_capability_reannounce_loop` re-broadcasts the
  node's capabilities every `MeshNodeConfig::capability_reannounce_interval`
  (default 150 s) with a 2×-interval TTL, refreshing both the local self-index (callee
  gate) and peers' folds (discovery). Re-broadcasting needs an owned `Arc` (the per-peer
  sends are `&self`), so `MeshNode::start_arc(self: &Arc<Self>)` stores a `Weak` the
  loop upgrades each tick; the SDK (`Mesh::start`) and FFI (`net_mesh_start`) call it. A
  bare `start(&self)` keeps its signature — no test-caller churn — and omits the loop
  (fine for the short-lived non-`Arc` nodes that take it). A/B tests pin both directions.
- **Bench-harness livelock** — `call_*_retrying` now bounds backpressure retries with a
  20 s `RETRY_DEADLINE`, so a saturated bar fails fast (`c128/32B` in ~24 s vs ~5 min)
  with `transport saturated … not measurable here`, instead of livelocking past a TTL.

---

## Explicit non-goals (don't spend time here)

- **AEAD algorithm swap** (ChaCha20-Poly1305 → AES-GCM): crypto is ~5% of CPU; saves
  1–2 µs at best and costs the portable-everywhere property. Ruled out in
  `PERF_AUDIT_2026_05_19_NRPC.md`; §3 (SIMD backend) captures the actual win.
- **Lock redesign**: replay window and per-shard mutexes are correct and uncontended.
- **Ring buffer / shard mapper / timestamp generator**: done (§7).
- **Moving decrypt off the recv loop**: ~5% win, not worth reordering flow control
  (already rejected in the QPS plan).
- **Binding internals rewrite**: the FFI layer is clean (§9); the win there is SDK
  docs/defaults, not code.
- **Consumer drain / merge**: clean (§11).
- **RedEX watcher batching in v1**: per-event live-tail delivery is the v1 model's
  ceiling by design; the fix is the v2 architecture, not a patch (§10).

---

## Recommended order of attack

1. **§1 gap-fix #1 (batched channel hop, ~40 LoC), then run the c128 measurement** —
   unblocks the recv-loop batching default and is the gate for the structural ceiling.
2. **§8a response-spawn elimination + §8b reply-channel cache** — contained diffs in
   `mesh_rpc.rs`, no wire change, directly attacks the wakeup count (8e); biggest
   bang-for-effort after §1.
3. **§4 event-id allocation** — small, unconditional, no protocol change; can land
   independently any time.
4. **§2 ack-piggyback** — the big unary lever (finishes what T1.1 started, see §8
   table); schedule as its own wire-change effort with cross-binding compat.
5. **§3 deployment decision on SIMD artifacts** — a build-pipeline decision, not code.
6. **§6/§8 micro-items (T3.4, T2.2, §8d metrics gate)** — opportunistic; re-bench
   T2.1 only after §1 lands.
7. **§9 SDK docs/defaults pass + §10 run `bench_append_batch_disk` + §14 annotate
   the FailureDetector rows in BENCHMARKS.md** — cheap, non-code, closes the record.
