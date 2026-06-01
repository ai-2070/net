# nRPC QPS — Concurrency Scaling Plan

> Investigates why `nrpc_qps` throughput scales **sub-linearly** with in-flight
> concurrency (c1 → c16 buys ~4×, not 16×). Companion to
> [`NRPC_FLAMEGRAPH.md`](NRPC_FLAMEGRAPH.md) and
> [`../misc/PERF_AUDIT_2026_05_19_NRPC.md`](../misc/PERF_AUDIT_2026_05_19_NRPC.md);
> reuses their T-item naming (T1.x/T2.x/T3.x). This plan is **diagnosis-first** —
> it does not commit to a fix until Phase 0 pins which stage is the wall.

## Observation

Bench: `nrpc_qps` (`net/crates/net/sdk/benches/nrpc_qps.rs`). Throughput metric is
`Throughput::Elements(concurrency)`, so each iteration fires `concurrency`
requests on one channel (`SVC_JSON`) via `FuturesUnordered`.

| bench       | latency / iter | throughput  | effective time / request |
|-------------|---------------:|------------:|-------------------------:|
| `c1/32B`    | 42.4 µs        | 23.6 K/s    | 42.4 µs                  |
| `c16/32B`   | 171.6 µs       | 93.3 K/s    | **10.7 µs**              |

16× the offered load → **~4× throughput**. The question: where is the 4× wall?

## Analysis — why it is non-linear

The server receive path is **not** one fully-serialized loop, but it **is** a
chain of single-consumer stages, and only the (trivial) handler runs parallel:

1. **One socket recv loop** — `MeshNode::spawn_receive_loop` (`adapter/net/mesh.rs:3144`)
   reads *every* inbound datagram from the single UDP socket.
2. **AEAD decrypt inline in that loop** — `process_local_packet` (`mesh.rs:3856`).
   All inbound decryption is serialized on that one task/core.
3. **Dispatch = non-blocking `try_send`** into a per-channel mpsc
   (`adapter/net/mesh_rpc.rs:1859`).
4. **One bridge task drains that mpsc** (`mesh_rpc.rs:1893`) and calls
   `fold.lock().apply(...)` — a **`Mutex`**. Single consumer + single lock **per
   channel**. The bench drives one channel, so all 16 in-flight requests funnel
   through the same bridge task and contend on the same mutex.
5. **Only then** does `RpcServerFold::apply` `tokio::spawn` the handler
   (`adapter/net/cortex/rpc.rs:1585`) → parallel.

For *this* bench the handler is `Ok(EchoResp { body: req.body })` — it does
essentially nothing, so the parallel stage (5) contributes ~zero. Real per-request
cost lives in the **serialized** stages: server recv + inline decrypt, the single
bridge task + `fold` mutex, response encrypt/send, and the **client's own single
recv loop + inline decrypt** of every response.

So the right mental model is a **pipeline of single-consumer stages**, not "16
requests over 4 cores." Concurrency helps only by keeping *different stages* busy
on different cores (pipeline parallelism). There are ~3–4 such stages (server
recv/decrypt → bridge/fold → client recv/decrypt), which is *also* why ~4× shows
up — and it partly coincides with the runtime's 4 worker threads
(`runtime()`, `nrpc_common/mod.rs:420`) **shared by both client and server**.

Two arithmetic checks, both landing at the same ceiling:
- Single-request critical path ≈ 42 µs (the c1 latency, since c1 can overlap
  nothing). `W=4` threads / 42 µs ≈ **95 K/s** — observed c16 is **93.3 K/s**.
- Equivalently, ~3–4 single-consumer pipeline stages each pinned to one core.

Either way the implication is the same and is the thing to verify:
**adding worker threads will plateau fast**, because the single recv loop, the
single bridge task, and the `fold` mutex are each one-core-bound regardless of
thread count.

This is consistent with `NRPC_FLAMEGRAPH.md`'s "scheduling/wake-latency bound, not
crypto-bound" finding (AEAD is invisible in the flame graph; 51 %
`NtWaitForAlertByThreadId`). The flamegraph profiled c1 latency; this plan
profiles the **c16/c128 ceiling** specifically.

## Goals

1. **Pin the binding constraint** for the c16/c128 ceiling: worker-thread count,
   single recv-loop + inline decrypt, or per-channel bridge + `fold` mutex.
2. Land the **cheapest fix that moves the ceiling** without regressing c1 latency
   or the streaming benches.
3. Leave behind a **bench variant + a documented number** so the scaling shape is
   regression-guarded, not re-derived by hand each time.

## Non-goals

- c1/32B single-shot latency — owned by `NRPC_FLAMEGRAPH.md` (T1.1/T1.2).
- Codec or discovery axes — owned by `nrpc_unary.rs`.
- Reliable-stream / bulk transfer — nRPC is not a bulk path (see memory note).

---

## Status

| Phase | State | Notes |
|---|---|---|
| 0 — Diagnose: which stage is the wall | ☐ Not started | Discriminating experiments below |
| 1 — Multi-channel shard bench variant | ☐ Not started | Settles per-channel bridge/mutex hypothesis |
| 2 — Fix the bottleneck Phase 0 names | ☐ Blocked on Phase 0 | Candidate fixes pre-scoped below |
| 3 — Re-bench + document the curve | ☐ Not started | Capture before/after; update this table |

---

## Phase 0 — Diagnose (no code changes to library hot path)

Run these **before** touching any fix. Each isolates one hypothesis. Capture
`nrpc_qps c1/32B`, `c16/32B`, `c128/32B`, and the `16KiB` rows each time.

### 0a — Worker-thread sweep (cheapest, do first)
Bump `runtime()` worker threads 4 → 8 → 16 on a ≥16-physical-core box.
- **Ceiling rises ~proportionally** → thread/CPU-bound; the serialized stages are
  *not* yet the wall at this thread count. Pursue T2.3 (inline handler) / general
  per-request CPU reduction.
- **Ceiling barely moves** → a single-consumer stage is the wall. Go to 0b/0c.

### 0b — Multi-channel shard (discriminating test) → see Phase 1
Spread the 16/128 in-flight calls across **N distinct service channels**
(`SVC_JSON_0..N`), each with its own bridge task + `fold` mutex.
- **Throughput jumps well above 93 K** → the **single bridge task / `fold` mutex
  per channel** (stage 4) is the binding constraint → fix = **T2.1**.
- **No change** → the bottleneck is upstream and shared by all channels: the
  **single recv loop + inline decrypt** (stages 1–2) → fix = move decrypt off the
  recv loop.

### 0c — Split client and server runtimes
Give `Pair` two runtimes (separate thread pools) instead of one shared 4-thread
runtime. If the absolute ceiling rises, the shared pool was artificially
depressing it (client recv loop + server recv loop fighting for the same cores) —
relevant for honest benchmarking even if not a library fix.

### 0d — Corroborating signals (grab alongside 0a–0c)
- **`top -H` / per-thread CPU under c16**: expect the recv-loop thread and/or the
  bridge thread pinned near 100 % of one core while others idle — the smoking gun
  for single-consumer serialization. Name the thread that saturates.
- **Payload sweep**: inline decrypt is borne by the single recv loop, so `16KiB`
  rows should fall off *harder* at high concurrency than CPU-parallel scaling
  predicts. If they do, that finger-points stage 2 (inline decrypt) specifically.
- **Re-profile under load**: re-record the flamegraph at `c16`/`c128` (not c1) and
  check whether `NtWaitForAlertByThreadId` share *drops* (more work, less idle) —
  if it stays ~51 %, we are still wake-latency bound even under saturation.

**Phase 0 exit:** a one-paragraph verdict naming the binding stage, backed by the
0b result + the per-thread CPU observation. Record it in the Status table.

---

## Phase 1 — Multi-channel shard bench variant

Implement 0b as a permanent bench so the per-channel hypothesis is measurable and
regression-guarded.

- New bench (or new group in `nrpc_qps.rs`): register `N` echo services on the
  server `Pair`, round-robin the `concurrency` in-flight calls across them.
- Axis: shards ∈ {1, 4, 16} at fixed `c16`/`c128`, `32B`/`1KiB`.
- Keep the existing single-channel bars (regression baseline) untouched.
- **Touches:** `nrpc_qps.rs` + `nrpc_common/mod.rs` (multi-service `Pair::new`).
  No library changes.

This gives the project a "channels vs throughput" curve that directly shows
whether sharding (a deployment-side mitigation) scales, independent of any
library fix.

---

## Phase 2 — Fix the bottleneck Phase 0 names

Pre-scoped candidates, mapped to existing audit T-items. **Implement only the one
Phase 0 selects.**

### If stage 4 (bridge task / `fold` mutex per channel) — **T2.1**
Drop the fold's outer `Mutex` + `in_flight: DashMap` (audit T2.1). Inbound REQUEST
crosses ~4 mutex regions today; under single-channel saturation this is exactly
when contention bites. Requires `RedexFold::apply` to take `&self` (interior
mutability) instead of `&mut self`, or a sharded lock.
- **Risk:** medium — touches the `RedexFold` trait signature. Needs the full redex
  fold test suite green.
- **Cheaper interim:** more bridge tasks per channel, or shard the channel
  (Phase 1 already proves whether this helps).

### If stages 1–2 (single recv loop + inline decrypt)
Move AEAD decrypt **off** the recv loop: the loop does the cheap counter/AAD admit
check, then hands the ciphertext to the spawned task (or a small decrypt worker
pool) where decrypt + decode happen in parallel. Keeps the loop doing only socket
drain + routing.
- **Risk:** medium-high — reorders the decrypt/`try_admit_rx_counter` sequence
  (`mesh.rs:3856`); replay-window admission must stay correct. Needs the transport
  replay/counter tests.
- **Synergy:** also lifts the c128 ceiling and helps the `16KiB` rows.

### Regardless of stage — bundle the cheap wins (from `NRPC_FLAMEGRAPH.md`)
- **T2.3** — inline the handler when its future is `Ready` instead of always
  `tokio::spawn` (`cortex/rpc.rs:1585`). The bench handler is synchronous; this
  removes one spawn+wake per call. ~1–3 µs/call, directly relevant since the
  spawn-storm dominates wall time.
- **T3.4** — gate `catch_unwind` behind a feature so the microbench path skips it.

These reduce per-request work on *every* stage, so they help the ceiling whichever
hypothesis wins. Bundle with the selected structural fix; re-bench between.

---

## Phase 3 — Re-bench + document

1. Re-run `nrpc_qps` full matrix + the Phase 1 shard variant, before/after.
2. Update the Status table and the Observation table with new numbers.
3. State the **scaling shape** explicitly: e.g. "c1 → c16 now N×; ceiling at
   c128/32B = X K/s; binding stage = Y." Future readers should not have to
   re-derive the 4× by hand.
4. If a deployment-side mitigation (channel sharding) is the practical answer
   rather than a library fix, document that in the SDK guidance, not just here.

---

## Risks & guardrails

- **Don't regress c1 latency.** Moving decrypt to the spawned task adds a hop on
  the single-request path; verify c1/32B does not climb (it is the headline
  latency number). The streaming benches (`nrpc_streaming.rs`) and
  `nrpc_qps c128/16KiB` are the regression tripwires named in `NRPC_FLAMEGRAPH.md`.
- **Replay-window correctness** if decrypt moves off the recv loop — counter
  admission order must be preserved. Transport replay tests gate this.
- **`RedexFold` trait churn** if T2.1 is chosen — wide blast radius; sequence it
  last and keep the redex fold suite green.
- **Bench honesty:** the shared single 4-thread runtime for both client and server
  (Phase 0c) means the *measured* ceiling is partly an artifact of the harness, not
  the protocol. Note this wherever the number is quoted.

## Open questions for Phase 0 to answer

- Is the wall the per-channel bridge/`fold` mutex (fixable by sharding **or**
  T2.1), or the shared recv-loop decrypt (only fixable in the library)?
- Does the spawn-per-request (stage 5) cost show up even with a no-op handler —
  i.e. is T2.3 worth it independent of the structural fix?
- How far does pure worker-thread count carry the ceiling before a single-consumer
  stage caps it? (0a quantifies the headroom.)
