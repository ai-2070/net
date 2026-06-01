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
| 0 — Diagnose: which stage is the wall | ✅ Done | **Verdict: shared single recv loop + inline decrypt.** Worker sweep flat; sharding doesn't help. See Findings below. |
| 1 — Multi-channel shard bench variant | ✅ Done | `nrpc_qps_shard` landed (`benches/nrpc_qps_shard.rs`); sharding *lowers* throughput → rules out per-channel bridge/mutex |
| 2 — Fix the bottleneck Phase 0 names | ◐ Scoped to HEAD; awaiting sign-off on T1.1 | T1.2 **already done**; T2.3/T3.4 won't move a syscall-bound ceiling. Only **T1.1 (skip unary StreamWindow grants)** hits the recv loop — but it changes credit accounting, so design is up for review before editing. Decrypt-off-loop withdrawn; T2.1 disproven |
| 3 — Re-bench + document the curve | ☐ Not started | Capture before/after once Phase 2 lands; update this table |

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

### Findings — 2026-06-01 (Windows 11, 24 logical cores)

Quick reads (criterion `--warm-up-time 1 --measurement-time 3 --sample-size 20`;
point estimate = median of the reported `[low mid high]`). These are diagnostic,
not the final published numbers — Phase 3 re-runs the full matrix.

**0a — worker-thread sweep** (`nrpc_qps`, `NRPC_BENCH_WORKER_THREADS`, 32 B):

| workers | c1/32B | c16/32B | scaling c1→c16 |
|--------:|-------:|--------:|---------------:|
| 4       | 24.2 K | 83.7 K  | 3.5×           |
| 8       | 22.2 K | 84.2 K  | 3.8×           |
| 16      | 22.9 K | 83.6 K  | 3.7×           |

The **c16 ceiling is invariant to worker count** (84 K ± noise across 4/8/16), and
c1 *drops* slightly as workers increase (more idle threads = more scheduling
overhead for a single in-flight call). If throughput were CPU-bound across the
worker pool, 16 workers would have lifted the ceiling well above 4 workers. It did
not. → **not thread/CPU-parallelism bound.** (This corrects the plan's original
"4 threads / 42 µs ≈ 95 K" framing: the ~4× is *not* load spread over 4 cores.)

**0b/1 — channel shard** (`nrpc_qps_shard`, c16/32B, 4 workers):

| shards | c16/32B | per-channel in-flight |
|-------:|--------:|----------------------:|
| 1      | 89.5 K  | 16                    |
| 4      | 80.7 K  | 4                     |
| 16     | 75.6 K  | 1                     |

Giving every request its **own** channel (own bridge task + own `fold` mutex)
makes throughput *worse*, not better — the opposite of what a per-channel
serialization bottleneck would show. The slight regression is consistent with more
channels = more bridge tasks competing for the same worker threads while the shared
upstream stage stays pinned. → **not per-channel bridge/mutex bound (T2.1 is not
the wall).**

### Verdict

Both experiments point the same way: the c16/c128 ceiling is set by a **single
shared-consumer stage upstream of dispatch** — the **one socket recv loop that does
inline AEAD decrypt** (`mesh.rs:3144` + `mesh.rs:3856`). It is pinned to one core
and every inbound request (and, symmetrically on the client, every response) must
pass through it. The observed ~4× c1→c16 gain is **pipeline parallelism** across
the few single-consumer stages (server recv/decrypt → bridge → client
recv/decrypt), which merely happened to sit near 4 — *not* CPU spread over 4
worker threads, and *not* relieved by sharding channels.

**Still owed (cheap, optional corroboration):** `top -H` / per-thread CPU under
c16 to *see* the recv-loop thread pinned near 100 % while peers idle. The two
throughput experiments already agree, so this is confirmation, not a gate.

#### What the recv-loop stage actually spends time on (course-correction)

Naming the *stage* (single recv loop) is not the same as naming the *cost*. Source
read of the loop + cross-reference to [`NRPC_FLAMEGRAPH.md`](NRPC_FLAMEGRAPH.md):

- The loop is **one `recv_buf_from` syscall at a time** (`transport.rs:280`, via
  `PacketReceiver`). A `BatchedPacketReceiver` (recvmmsg) exists but is **Linux-only
  and not wired into `spawn_receive_loop`**; the bench host is Windows, which has no
  recvmmsg.
- Flamegraph bucket split: **`NtWaitForAlertByThreadId` ~51 %** (wakeups /
  park-unpark), **transport syscalls ~22 %** (`sendto` + IOCP), **AEAD decrypt
  ~5 %** ("crypto is invisible in the flame graph").

So the recv-loop stage is **syscall- and wake-latency-bound, not decrypt-bound.**
This **invalidates the Phase 2 branch the verdict first selected** ("move decrypt
off the recv loop"): at ~12 µs/packet on the loop, decrypt is ~0.6 µs — removing it
buys ~5 % (~84 K → ~88 K) and is not worth reordering replay-counter admission for.
The real levers reduce **syscalls and wakeups per round trip** — see the rewritten
Phase 2.

> Note: c128/32B point estimates were not captured cleanly — at 128 in-flight the
> bench can't fit 20 samples into a 3 s window (criterion warns and the summary
> line format shifts). Phase 3 will give c128 a longer measurement window. It does
> not affect the verdict, which rests on the c16 ceiling and the shard curve.

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

The binding stage is the single shared recv loop (Phase 0). But the scoping read
above shows that stage is **syscall/wake-bound, not decrypt-bound** — so the fix is
to cut **syscalls and wakeups per round trip**, *not* to move decrypt. Levers
below, ranked by value/risk after the course-correction.

### Implementation scoping — current code state (2026-06-01)

The codebase advanced past the flamegraph audit; re-checking each lever against
HEAD before writing any code:

- **#2 T1.2 — already implemented.** `publish_response_to_caller`
  (`mesh_rpc.rs:1561`) resolves `caller_origin` → node (bridge cache `target_hint`,
  else `get_node_by_origin_hash`) and ships the response via `publish_to_peer`,
  falling back to roster `publish` only when origin is unknown. All four response
  emitters (`mesh_rpc.rs:1794/2034/2152/2388`) route through it. **No work left.**
- **#3 T3.4** (gate `catch_unwind`, `cortex/rpc.rs:1613`) is isolated but
  ~100–300 ns/call — invisible against a syscall-bound ceiling. **T2.3** (inline
  ready handler) is *not* cheap: it entangles the `in_flight` registration,
  cancel-wins ordering, panic scope, and metrics (`cortex/rpc.rs:1562-1625`), and it
  would run handler bodies on the single bridge task — risking *serialization* of
  the very work the spawn exists to parallelize. Neither moves the c16 ceiling.
- **#1 T1.1 — the only lever that touches the bottleneck, and it is real
  flow-control work.** The StreamWindow grant path runs **inline on the recv loop**
  (`mesh.rs:4652-4725`): every accepted packet → `on_bytes_consumed` → peer resolve
  → `pending_stream_grants.lock()` → `notify_one`, and later a grant packet on the
  wire (recv-loop work on *both* ends). For unary (one packet each way) this is pure
  overhead. But `on_bytes_consumed` (`session.rs:1077-1106`) **refills credit 1:1 on
  every packet by design** (bumps `granted` and `consumed` together to hold
  `outstanding` at `window_bytes`). Skipping grants on unary therefore means
  changing the credit-accounting cadence — safety-relevant (credit is kernel-buffer
  DoS protection) and capable of stalling *streaming* if mis-tuned. **Needs a
  designed threshold + `nrpc_streaming.rs` as the gate; not a casual edit.**

**Status:** stopping here for sign-off on the T1.1 approach before modifying
flow-control accounting. Proposed design below.

#### Proposed T1.1 design (for review)

- Add a per-`RxCreditState` "consumed-since-last-grant" accumulator. Emit a grant
  only when it crosses a fraction of `window_bytes` (start conservative, e.g.
  **½ window**); otherwise bump bookkeeping and return `None` (no grant enqueued).
- Wire-safe because grants are **authoritative + self-healing** (each carries full
  `total_consumed`; `session.rs:1069-1076`): skipping intermediate grants is
  already tolerated, a later grant reconciles. The sender only stalls if its credit
  reaches 0 before a grant arrives — so the threshold must be **strictly below** the
  window so a grant is always in flight before the sender drains.
- Unary (consumes ≪ window in one packet) emits **zero** grants → removes the
  per-request lock + notify + grant packet from both recv loops.
- **Gate:** `nrpc_streaming.rs` (no stall / no throughput regression) +
  `nrpc_qps c16/32B` (ceiling should rise) + existing session/stream credit tests.
- **Rollback:** single accumulator + one comparison; revert is trivial.

> **Superseded:** the verdict's first instinct ("move decrypt off the recv loop")
> is now a **non-lever (~5 %)** and removed from the critical path. See "Rejected"
> at the end of this section. T2.1 (per-channel fold mutex) was already disproven by
> the shard test.

### #1 — Coalesce StreamWindow grants on unary (T1.1) — **biggest win**
`NRPC_FLAMEGRAPH.md` confirms (from source, `session.rs:1012-1041` + `mesh.rs:4399`)
that every accepted inbound data packet fires a StreamWindow grant — so a unary
round trip carries **2 extra grant packets**, each = 1 `sendto` + 1 recv-loop
wakeup + 1 AEAD on *both* ends. Coalescing/skipping grants on unary directly
removes packets from the single recv loop — the exact resource Phase 0 named — so
it should lift the c16/c128 ceiling, not just c1.
- **Risk:** medium — touches flow control; the streaming benches
  (`nrpc_streaming.rs`) + `nrpc_qps c128/16KiB` must stay green.
- **Synergy:** this is already the #1 item in `NRPC_FLAMEGRAPH.md`; this plan adds
  the throughput-ceiling justification on top of the latency one.

### #2 — Response leg → `publish_to_peer` direct (T1.2) — **low-risk**
Response path uses `mesh.publish` (roster fan-out + ACL + subnet filter + a
`Vec<Bytes>` alloc) when `caller_origin` already pins the target node. Switching to
`publish_to_peer` removes that per-response overhead. Mechanical, ~4 sites
(`mesh_rpc.rs:1510,1753,1964,2066,2286`). Low risk.

### #3 — Drop spawn+wake per call (T2.3 + T3.4) — **cheap**
- **T2.3** — inline the handler when its future is `Ready` instead of always
  `tokio::spawn` (`cortex/rpc.rs:1585`). The echo handler is synchronous; removes
  one spawn+wake per call — directly attacks the 51 % wake-latency bucket.
- **T3.4** — gate `catch_unwind` behind a feature so the microbench path skips it.

### #4 — Parallelize/batch the recv syscall — **structural, platform-split**
The deepest fix for the named stage, but the most involved:
- **Linux:** wire the existing `BatchedPacketReceiver` (recvmmsg) into
  `spawn_receive_loop` so one syscall drains many datagrams — amortizes the ~22 %
  syscall bucket. The code exists (`transport.rs:297`, `linux.rs`); it is just not
  on the live receive path.
- **Windows (the bench host):** no recvmmsg. Options are registered I/O (RIO) or
  multiple recv loops on `SO_REUSEPORT`-style sockets — both change the one-node /
  one-socket model and are out of scope until #1–#3 are measured.
- **Risk:** high; defer until #1–#3 land and Phase 3 re-bench shows residual
  headroom against the syscall wall.

### Rejected — move decrypt off the recv loop
Originally selected by the Phase 0 verdict; **withdrawn**. Decrypt is ~5 % of the
loop's time (`NRPC_FLAMEGRAPH.md`: "crypto is invisible"). Moving it would buy
~5 % while reordering the `decrypt → try_admit_rx_counter` replay-admission
sequence (`mesh.rs:3892-3900`) and risking a per-packet unbounded-spawn DoS amp.
Bad value-for-risk; not doing it.

**Sequence:** #2 (mechanical, low risk) → #3 (cheap) → re-bench → #1 (biggest, needs
streaming bench green) → re-bench → reassess #4 only if a syscall wall remains.

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

- **Don't regress c1 latency.** c1/32B is the headline latency number; verify it
  does not climb after any change. The streaming benches (`nrpc_streaming.rs`) and
  `nrpc_qps c128/16KiB` are the regression tripwires named in `NRPC_FLAMEGRAPH.md`.
- **Flow-control correctness (T1.1, lever #1).** Coalescing/skipping StreamWindow
  grants must not starve *streaming* traffic of window credit — only unary (single
  packet each way) is safe to skip. `nrpc_streaming.rs` is the gate.
- **Withdrawn risk — decrypt reorder.** The earlier "decrypt off the recv loop"
  branch carried a replay-window-ordering risk and a per-packet unbounded-spawn DoS
  amp; both are moot now that the branch is rejected (see Phase 2 "Rejected").
- **`RedexFold` trait churn (T2.1)** is also off the table — T2.1 was disproven as
  the wall by the shard test, so its wide-blast-radius trait change is not needed.
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
