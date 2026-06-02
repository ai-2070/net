# nRPC Send Loop — Batching (sendmmsg) Plan

> Evaluates batching the router's UDP **send** path (one `send_to` per packet →
> one `sendmmsg`/drain per burst) as a lever for `nrpc_qps`. **Diagnosis-first:**
> the headline finding is that send-side batching does **not** move `nrpc_qps` as
> it stands, because that bench is latency-bound with send-queue depth ≈ 1 — and
> the c16 ceiling is already pinned to the *receive* loop, not the send loop.
> Companion to
> [`NRPC_QPS_CONCURRENCY_SCALING_PLAN.md`](NRPC_QPS_CONCURRENCY_SCALING_PLAN.md)
> (which owns the c16/c128 ceiling and proves the wall is the shared recv loop),
> [`NRPC_FLAMEGRAPH.md`](NRPC_FLAMEGRAPH.md) (wake-latency-bound, not crypto-bound),
> and [`NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md`](NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md)
> (the wire change that actually removes packets/round-trip).

## Origin

A reading of the `nrpc_qps` numbers raised `sendmmsg` as a "cleanest fix in
terms of impact-to-implementation-effort," on the premise that per-packet send
syscall cost dominates a 32-byte workload and batching 32–128 packets/syscall
buys 10–20×. That premise is correct **for a saturated one-way send**, but does
not hold for `nrpc_qps`, which is a request/response latency bench. This plan
records why, the cheap measurement that settles it, and where send-batching *is*
worth landing (a different, honest bench).

## Observation

| bench       | latency / iter | throughput  | effective time / request |
|-------------|---------------:|------------:|-------------------------:|
| `c1/32B`    | 42.4 µs        | 23.6 K/s    | 42.4 µs                  |
| `c16/32B`   | 171.6 µs       | 93.3 K/s    | **10.7 µs**              |

Bench: `nrpc_qps` (`net/crates/net/sdk/benches/nrpc_qps.rs`), axes
`CONCURRENCY = [1, 16, 128]` × `PAYLOADS = [32B, 1KiB, 16KiB]`. Each iteration
fires `concurrency` calls into a `FuturesUnordered` and **awaits the responses**,
so a sample is a full request→response round trip and `Throughput::Elements`
counts requests/sec.

The send loop (`adapter/net/router.rs:645-696`) is one packet per iteration:

```rust
if let Some(packet) = scheduler.dequeue() {        // router.rs:648
    let _ = socket.send_to(&packet.data, packet.dest).await;  // router.rs:659 — one syscall/packet
} else {
    tokio::select! { _ = scheduler.wait() => {}, _ = sleep(1ms) => {} }
}
```

## Why sendmmsg does not move `nrpc_qps`

`sendmmsg` amortizes syscall cost **only when packets are already backlogged in
the scheduler at dequeue time.** That is the shape of a saturated one-way blast,
not of a low-queue-depth request/response bench:

- **c1: send-queue depth is 1.** Exactly one RPC is in flight; the loop sends the
  request, then blocks awaiting the response. A batched send would call
  `sendmmsg` with vector length 1 — i.e. a more expensive `send_to`. The 42.4 µs
  is the round-trip wake chain (enqueue → `notify_one` → send-loop wake →
  `send_to` → loopback → recv-task wake → dispatch → handler → response enqueue →
  wake → send → recv → future resolves; ~6–8 tokio wakeups). A loopback `sendto`
  is ~1–2 µs of that. **Batching saves zero at c1.**
- **c16: up to 16 packets *can* burst together,** so a drain could coalesce a few
  syscalls — but the win is bounded by ~16 × ~1–2 µs of syscall overhead spread
  across a 171 µs window dominated by the same wake chain, and only when the burst
  actually aligns at the dequeue. Marginal, and not the wall.

Crucially, the companion plan already **localized the c16/c128 ceiling to the
shared single recv loop + inline decrypt** (worker-thread sweep flat at ~84 K;
channel-sharding made it *worse*). The send loop is downstream of that wall, so
even a perfect send-side batch cannot lift the measured ceiling. The "10–20× on
32-byte packets" figure is real but belongs to a **saturated one-way send**
microbench — a workload `nrpc_qps` does not contain. Implementing sendmmsg and
re-running `nrpc_qps` would show ~no change and read as "the optimization
failed," when the truth is it was never exercised.

## The batch path already exists (and is not on the send loop)

`sendmmsg` is **already wired** for Linux, just not on the router's send loop:

- `PacketSender::send_batch` (`adapter/net/transport.rs:497`, `cfg(target_os =
  "linux")`) → `linux.rs:126-286`, `MAX_BATCH_SIZE = 64` (`linux.rs:50`), real
  `libc::sendmmsg` FFI with partial-send tail retry.
- Symmetric receive side `BatchedPacketReceiver` (recvmmsg,
  `transport.rs:302`, Linux-only) also exists and is **also not** wired into the
  live receive loop (noted in the companion plan).

So the gap is not "sendmmsg is missing." It is that **the send loop is not
batch-shaped**, and at `nrpc_qps`'s queue depth there is nothing to batch.

**The fd plumbing is clean.** `send_batch` takes `&self`, borrows the fd from the
shared `Arc<UdpSocket>` via `AsRawFd`, and builds a `BatchedTransport::new_send_only(fd)`
internally (`transport.rs:497-503`) — it does **not** own a separate socket. So the
router can call `sendmmsg` against its *existing* socket fd; there is no
fd-compatibility unknown. Two real caveats remain, both addressed in Phase 1:

- **Per-call allocation.** `PacketSender::send_batch` rebuilds a `BatchedTransport`
  (3 × `Vec::with_capacity(64)`) on *every* call — the doc comment says it must,
  because a shared `PacketSender` would otherwise need a lock on the hot path
  (`transport.rs:492-496`). The router's send loop is a *single task*, so it can
  sidestep this by owning one `BatchedTransport` and reusing it (no lock).
- **tokio-readiness bypass.** `send_batch` is a synchronous `sendmmsg` on the
  non-blocking fd; it returns `EWOULDBLOCK` instead of providing the backpressure
  `send_to().await` gives. Needs an async fallback (see Risks).

## Goals

1. **Settle the premise with one cheap measurement** — instrument send-queue
   depth at dequeue under `nrpc_qps` and confirm depth ≈ 1 (c1) / small-bursty
   (c16). 10 minutes; ends the debate without speculative code.
2. **Land send batching where it is honestly demonstrable** — add a saturated
   one-way throughput bench and (Linux) wire `send_batch` into a batch-shaped
   drain so the 10–20× claim has a real home and a regression guard.
3. **Record the multi-send-loop option and its fairness hazard** so it is not
   re-proposed without the scheduler redesign it actually requires.

## Non-goals

- The **c16/c128 ceiling** and the recv-loop wall — owned by
  [`NRPC_QPS_CONCURRENCY_SCALING_PLAN.md`](NRPC_QPS_CONCURRENCY_SCALING_PLAN.md).
- **Ack-piggyback / packets-per-round-trip** — owned by
  [`NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md`](NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md).
- **c1/32B single-shot latency** — owned by
  [`NRPC_FLAMEGRAPH.md`](NRPC_FLAMEGRAPH.md).
- macOS/Windows batched send — out of scope; this is explicitly a Linux send-path
  optimization (see Cross-platform).

---

## Status

| Phase | State | Notes |
|---|---|---|
| 0 — Measure send-queue depth at dequeue | ☐ Todo | **Not a c1-safety gate** (the Phase 1 skip handles c1) — it (a) confirms the batch branch *fires* on real traffic rather than being born cold, and (b) sizes the typical batch N. Expectation: depth ≈ 1 at c1, small/bursty at c16/c128. |
| 1 — Conditional-skip batch drain in the send loop | ☐ Todo | Guard on the cheap `total_queued()` atomic: depth ≤ 1 → `send_to` (unary path unchanged); else drain up to N and one `send_batch` (Linux) / `send_to` loop (portable). Loop-owned reusable `BatchedTransport` (single consumer ⇒ no lock, no per-call alloc). Helps saturated/streaming bursts, ~nothing for unary `nrpc_qps`. |
| 2 — Saturated one-way send bench | ☐ Todo | New `nrpc_send_throughput` (fire-and-forget, no response await) — the honest home for the 10–20× sendmmsg claim and its regression guard. Keep `nrpc_qps` as the latency story. |
| 3 — Multi-send-loop option (documented, not built) | ☐ Analysis only | Cross-platform alternative to sendmmsg, but breaks the FairScheduler's advertised property — see hazard below. Treat as scheduler redesign, not drop-in. |

---

## Phase 0 — Measure send-queue depth (sizing, not safety gate)

The Phase 1 conditional skip already keeps c1 safe, so this is **not** a gate on
correctness. It answers two questions cheaply before paying the Linux-`cfg` +
`EWOULDBLOCK` surface: (a) does the batch branch ever *fire* on real traffic, or
is the send queue structurally depth-1 because the recv-loop wall throttles
arrivals upstream — i.e. would the path be born cold? and (b) what is the typical
burst N, which decides whether the loop-owned reusable transport (Phase 1) is
worth it.

- Add a transient counter/histogram at `router.rs:648`: sample
  `scheduler.total_queued()` (already exists, `router.rs:326`) at the moment
  `dequeue()` returns `Some`, or count consecutive `Some` returns before the loop
  next blocks on `wait()`. Behind a `cfg(feature)` or an env gate so it never
  ships in the hot path.
- Run `nrpc_qps c1/32B`, `c16/32B`, `c128/32B`.
- **Expected:** depth ≈ 1 at c1; small bursts (≤ in-flight) at c16/c128 that only
  occasionally exceed 1 at the dequeue instant. → send batching is not an
  `nrpc_qps` lever; if the branch is effectively cold even at c128, Phase 1 is a
  streaming/saturated optimization only and may not be worth the surface — proceed
  to Phase 2 (honest bench) to find a workload that does backlog.
- **If instead** depth is consistently deep at c16/c128 → the burst *does* pile up,
  Phase 1 becomes a real `nrpc_qps` lever, and the typical depth sizes N (and
  justifies the reusable transport); re-rank accordingly.

**Phase 0 exit:** a one-line verdict with the measured depth distribution + the
typical batch N, recorded in the Status table. Cheap, and it sizes the rest.

---

## Phase 1 — Conditional-skip batch drain in the send loop

The right shape is **conditional skip**: batch when packets are already waiting,
fall through to today's single `send_to` when they are not — so the unary path is
untouched and the win is captured automatically whenever a burst exists. The two
details that make it good rather than merely correct are the **depth probe** and
**transport reuse**.

```text
loop:
  let packet = scheduler.dequeue()? ;                  // first packet (today's path)
  if scheduler.total_queued() <= 1 {                   // cheap atomic load — NOT a 2nd dequeue()
      socket.send_to(&packet.data, packet.dest).await; // unary fast path, ~unchanged cost
  } else {
      drain up to N more via dequeue() into batch       (N = MAX_BATCH_SIZE = 64)
      #[cfg(linux)]      batched_transport.send_batch(&batch, dest)   // one sendmmsg, reused transport
      #[cfg(not(linux))] for p in &batch { send_to(p).await }         // portable fallback
      // on EWOULDBLOCK from sendmmsg: fall back to async send_to(p).await for backpressure
  }
  if drained nothing -> select! { wait(), sleep(1ms) }
```

- **Probe with the atomic, not a speculative `dequeue()`.** Deciding batch-vs-single
  by calling `dequeue()` again would allocate a `Vec<u64>` of stream keys per call
  (`router.rs:245`) and scan the DashMap — that puts an allocation on the unary
  path, regressing the c1 tripwire. `total_queued()` (`router.rs:326`) is a single
  `AtomicU64` load: the skip stays near-free, which is what makes "just wire it in"
  actually safe for c1. (Caveat: `total_queued` counts *all* streams, and the WRR
  quantum may not let one drain pull them all in a single pass; that only means an
  occasional batch is smaller than the count suggested — never a correctness issue.)
- **Loop-owned reusable `BatchedTransport`.** Construct one
  `BatchedTransport::new_send_only(fd)` when the send loop starts and reuse it every
  drain. The send loop is a *single task*, so the lock that forced
  `PacketSender::send_batch` to rebuild per call (`transport.rs:492-496`) is
  unnecessary here — this avoids the 3 × `Vec::with_capacity(64)` allocation per
  batch, which matters most at small bursts where it would otherwise eat the
  syscall saving.
- **Collapses both syscalls *and* send-loop wakeups** when a burst is present (one
  drain handles many packets instead of re-entering the loop per packet) — the
  wakeup collapse is the more interesting half given the 51 %
  `NtWaitForAlertByThreadId` bucket from the flamegraph.
- **Degrades to today's behavior at depth ≤ 1**, so c1 latency is not regressed
  (verify — c1/32B is a tripwire).
- **EWOULDBLOCK fallback (Linux).** The synchronous `sendmmsg` on the non-blocking
  fd bypasses tokio's writability; on a full send buffer it returns `EWOULDBLOCK`.
  The drain must fall back to async `send_to().await` for the unsent tail, never
  spin or drop.
- **Test-only loss injection** (`router.rs:649-658`, `drop_every_n`) must be
  applied per-packet *inside* the batch, not per-drain, or the simulated-loss
  tests change meaning.

This is the lever the original sendmmsg suggestion was reaching for — correctly
scoped to where backlog exists, not to unary `nrpc_qps`.

### Cross-platform

`sendmmsg` is Linux-only; macOS has no direct equivalent and falls back to the
portable `send_to` loop — i.e. the substrate sends faster on Linux than macOS
under saturation. For the deployment target (Linux) this is acceptable; state it
explicitly wherever the number is quoted so the macOS gap is not a surprise.

---

## Phase 2 — Saturated one-way send bench (the honest home)

Give the 10–20× sendmmsg claim a workload that actually exercises it, separate
from the latency bench:

- New bench `nrpc_send_throughput` (or a group in `nrpc_qps.rs`): enqueue N 32-byte
  packets fire-and-forget (no response await), measure packets/sec drained by the
  send loop. This is where the send queue is deep and `sendmmsg` shows its win.
- Axes: payload `32B`/`1KiB`, batch on/off (so the bench is also the before/after
  for Phase 1).
- Keep `nrpc_qps` untouched as the latency/round-trip story; document that the two
  benches measure different things so a future reader does not expect sendmmsg to
  move `nrpc_qps`.

---

## Phase 3 — Multi-send-loop option (documented, not built)

The cross-platform alternative to sendmmsg: spawn N send tasks each pulling from
the scheduler. **It breaks the FairScheduler's advertised property** and is a
scheduler redesign, not a drop-in:

- `dequeue()` reads the rotation cursor then commits it in two steps:
  `round_robin_idx.load(...)` for `start` (`router.rs:264`) and a separate
  `fetch_add(1)` only inside the successful-pop arm (`router.rs:279`, mirrored at
  `:310`). With N concurrent `dequeue()` calls, the gap between the read and the
  `fetch_add` is **not atomic across loops**: two loops can read the same `start`,
  service the same stream offset, and race the per-stream quantum accounting
  (`sent_this_round` / `increment_sent`, `router.rs:274-276`).
- The specific regression that fires is
  `round_robin_idx_advances_only_on_successful_pop` (the #31 pin in
  `router.rs`'s test module) and the WRR weight test
  (`test_fair_scheduler_respects_stream_weight`) — they would flake, not crash.
  The fairness *property the scheduler claims* stops holding.
- **Prerequisite if ever pursued:** make rotation-cursor advance atomic with the
  pop decision (e.g. CAS the cursor, or shard the scheduler per send-loop with a
  fairness model that accounts for N consumers), then run the full fairness suite
  green **before** committing. Until then, sendmmsg (single loop, Phase 1) is the
  lower-risk path because it keeps one consumer on the scheduler.

---

## Risks & guardrails

- **Don't regress c1 latency.** c1/32B is the headline latency number; the Phase 1
  drain must keep the depth-1 path identical to today's single `send_to`. Verify
  c1/32B does not climb. Streaming benches (`nrpc_streaming.rs`) and
  `nrpc_qps c128/16KiB` are the standing tripwires.
- **Don't claim a `nrpc_qps` win from send batching.** Phase 0 is the guard: if
  depth ≈ 1, any `nrpc_qps` delta from Phase 1 is noise. Report the saturated
  bench (Phase 2) for the sendmmsg number, not `nrpc_qps`.
- **tokio readiness bypass (Phase 1, Linux).** Raw-fd `sendmmsg` sidesteps tokio
  writability; `EWOULDBLOCK` must fall back to async `send_to().await`, never spin.
- **Fairness property (Phase 3).** Multi-send-loop without an atomic cursor
  advance silently breaks the scheduler's fairness guarantee — fairness tests are
  the gate, and they flake rather than fail loudly, so this needs deliberate
  verification, not a green CI by luck.
- **Bench honesty.** As in the companion plan, the shared single runtime for
  client+server colors absolute numbers; note it wherever quoted.

## Open questions for Phase 0 to answer

- What is the actual send-queue depth distribution at the dequeue under c1 / c16 /
  c128? (Settles whether Phase 1 is ever a `nrpc_qps` lever.)
- Does any realistic nRPC workload (vs. a synthetic blast) backlog the send queue
  enough for batching to matter, or is the send loop structurally depth-1 because
  the recv-loop wall upstream throttles arrivals before they pile up?
