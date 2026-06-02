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
microbench — a workload `nrpc_qps` does not contain.

## ⚠️ Lead finding (verified 2026-06-02): the send loop is NOT on the unary path

The premise behind "wire `send_batch` into the send loop" assumes unary RPC sends
flow through the `FairScheduler`/`NetRouter` send loop (`router.rs:645`). **They do
not.** Code trace of the `nrpc_qps` path:

- **Request:** `MeshNode::call` (`mesh_rpc.rs:3004`) → `publish_to_peer`
  (`mesh.rs:8231`) → `self.socket.send_to(&packet, next_hop).await` **directly**
  (`mesh.rs:8342`). No `scheduler.enqueue`.
- **Response:** `publish_response_to_caller` (`mesh_rpc.rs:1561`) → the same
  `publish_to_peer` → direct `socket.send_to` (`mesh_rpc.rs:1574`).
- **The scheduler send loop only drains *scheduled bulk-transfer streams*** —
  `deliver_stream_packet(scheduled=true)` → `router.scheduler().enqueue(...)`
  (`mesh.rs:10710-10749`). Unary RPC never sets `scheduled`, so it takes the
  `else` branch: direct `socket.send_to`. The loop is *running* (started at
  `mesh.rs:2784`) but sees **zero unary traffic**.

**Consequence — the original framing is void:** there is **no shared send queue on
the unary path** to batch. At c128, 128 concurrent response handlers each issue
their *own* `socket.send_to` from their own task; the packets are never funneled
through one egress where `sendmmsg` could coalesce them. So send-loop batching does
nothing for `nrpc_qps` — not because depth is 1, but because **the loop isn't on
the path**. Applying sendmmsg to unary would require **first introducing an egress
aggregation point** (route `publish_to_peer` through a batching queue, or add one),
which is a real architectural change — *not* the localized "wire in the existing
`send_batch`" the impact-to-effort pitch assumed. That pitch does not survive
contact with the code.

This **re-scopes the entire plan**: send-loop batching (Phase 1) is valid **only
for the scheduled bulk-transfer / streaming egress** that actually uses the
scheduler — never for unary `nrpc_qps`. The unary QPS lever stays where the
companion plans put it: **ack-piggyback** (fewer packets/round trip) and **batched
recv**, not send batching.

## The batch path already exists (for the scheduled-stream egress)

`sendmmsg` is **already wired** for Linux, just not invoked from the send loop:

- `PacketSender::send_batch` (`adapter/net/transport.rs:497`, `cfg(target_os =
  "linux")`) → `linux.rs:126-286`, `MAX_BATCH_SIZE = 64` (`linux.rs:50`), real
  `libc::sendmmsg` FFI with partial-send tail retry.
- Symmetric receive side `BatchedPacketReceiver` (recvmmsg,
  `transport.rs:302`, Linux-only) also exists and is **also not** wired into the
  live receive loop (noted in the companion plan).

So for the **scheduled-stream path** the gap is not "sendmmsg is missing" — it is
that the send loop is not batch-shaped. (For the **unary path** the gap is the
lead finding above: there is no send queue there at all.)

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

1. **Record the lead finding** so no one re-pitches "wire `send_batch` into the
   send loop" as an `nrpc_qps` lever: the unary path bypasses the scheduler send
   loop and has no shared egress queue to batch.
2. **Land send batching where it is real** — the **scheduled bulk-transfer /
   streaming egress** (the only path that uses the scheduler) — and prove it with a
   saturated one-way bench so the 10–20× claim has an honest home + regression guard.
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
| **Lead finding — send loop off the unary path** | ✅ Done (code trace 2026-06-02) | Unary RPC sends bypass the scheduler: `call`→`publish_to_peer`→`socket.send_to` (`mesh.rs:8342`); responses same (`mesh_rpc.rs:1574`). Scheduler send loop drains only `scheduled=true` streams (`mesh.rs:10710`). **Send-loop batching cannot move `nrpc_qps`.** Re-scopes Phases 0/1 to the scheduled-stream egress. |
| 0 — Measure drain depth on the **scheduled-stream** egress | ✅ Done (measured 2026-06-02, macOS) | **Verdict: drain depth ≈ 1 even for a saturated, backpressure-disabled single scheduled stream.** 4,580 / 4,589 drain runs were length 1 (99.8%); longest run 4–7. Producer's per-packet build+encrypt never outruns the loop's per-packet `send_to`. See Findings. |
| 1 — Batch drain in the scheduled-stream send loop | ✅ Implemented & **Linux-validated (CI green)** | Added `FairScheduler::current_depth`; send loop: depth 0 → single `send_to` (unchanged), else drain ≤64 grouped **by destination** → one `send_batch`/peer (Linux) or per-packet (portable). **CI on ubuntu-latest compiled + ran the `cfg(linux)` sendmmsg path: 61.9 packets/`sendmmsg` ≈ 62×** on real syscalls. Fairness + grouping + depth unit tests + single-stream fast path green. **Delivery integrity closed:** `scheduled_stream_integrity` fetches blobs over reliable scheduled streams, asserts byte-for-byte through the batch path (16/16 locally), CI-gated on Linux — including a **tiny-send-buffer phase** that forces the `sendmmsg` partial-send / `EWOULDBLOCK` tail-fallback under verification. No material gaps left; deferred follow-ups only (reusable transport, slot pruning, wakeup-collapse latency). |
| 2 — Saturated one-way send bench | ☐ Todo | New `nrpc_send_throughput` (fire-and-forget over a **scheduled stream**, no response await) — the honest home for the 10–20× sendmmsg claim and its regression guard. Keep `nrpc_qps` as the latency story. |
| 3 — Multi-send-loop option (documented, not built) | ☐ Analysis only | Cross-platform alternative to sendmmsg, but breaks the FairScheduler's advertised property — see hazard below. Treat as scheduler redesign, not drop-in. |
| — Unary `nrpc_qps` lever (out of scope here) | ➜ Elsewhere | Not send batching. See [`NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md`](NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md) (fewer packets) + batched recv in [`NRPC_QPS_CONCURRENCY_SCALING_PLAN.md`](NRPC_QPS_CONCURRENCY_SCALING_PLAN.md). |

---

## Phase 0 — Measure drain depth on the scheduled-stream egress

The lead finding already settled the unary question by code trace: the send loop
sees **no** `nrpc_qps` traffic, so measuring under `nrpc_qps` would correctly show
an idle loop and prove nothing new. Phase 0 is therefore about the *only* workload
that uses the loop — **scheduled bulk-transfer streams** — to size batch N before
paying the Linux-`cfg` + `EWOULDBLOCK` surface.

- **Do not use `total_queued()` — it is a cumulative enqueue counter, not current
  depth.** It is `fetch_add` on enqueue (`router.rs:201,214`) and **never
  decremented** (no `fetch_sub` in the file; `tests/scheduled_stream.rs:8`
  documents this as intentional). Reading it as depth would be `<= 1` only for the
  first packet ever, then false forever.
- **Instrument = drain run-length, local to the send loop.** Count consecutive
  `dequeue() → Some` returns before the loop next falls through to `wait()`, and
  record the run-length distribution (env-gated, e.g. `NET_SEND_DEPTH_HISTO`). No
  scheduler change; directly measures *batchability*.
- **Drive a scheduled-stream workload**, not `nrpc_qps`: a bulk transfer over a
  stream with `config.scheduled = true` (`mesh.rs:10549`) — e.g. extend the
  existing `transfer_concurrency` / `scheduled_stream` tests, or a new bench that
  blasts a multi-MB payload so the per-stream queues actually back up.
- **Expected:** under a saturated scheduled bulk transfer the run-length climbs
  toward `MAX_BATCH_SIZE` (64) — that is where `sendmmsg` pays. If even a saturated
  bulk stream stays at run-length ≈ 1 (because credit/window flow-control drip-feeds
  the scheduler one packet at a time), then send batching has *no* live workload in
  the codebase today and Phase 1 should be deferred until one exists.

**Phase 0 exit:** a one-line verdict with the measured run-length distribution
under bulk streaming + the typical N, recorded in the Status table.

### Findings — 2026-06-02 (macOS, loopback, 4 worker threads)

Instrument: an armed-once drain-run histogram in the send loop
(`router.rs`, `arm_send_drain_histo` / `send_drain_histo_snapshot`); driver:
`tests/send_drain_depth.rs`. Workload: 40 bursts × 800 × 1 KiB events on one
`scheduled=true` stream with `window_bytes=0` (backpressure disabled), batched
~7 events/packet → **4,600 packets** through the `FairScheduler`.

**Result A — single saturated stream (1 producer), 4,600 packets:**

| drain run-length | count | share |
|---|---:|---:|
| **1**       | 4,600 | **100 %** |
| ≥2          | 0     | 0 % |

`backpressure=0` (never WindowFull, never queue-full) — the *best case* for
backlog: the producer was never throttled and **still** could not get ahead of
the drain loop. The producer pays a per-packet `build` (AEAD encrypt of the
~7-event batch) + credit acquire + retransmit-register *inline* before each
`enqueue`, which costs ≳ the loop's per-packet `send_to` on loopback — so
`notify_one` → drain-one → `wait()` runs 1:1 and the queue never accumulates.
Empirically confirms the in-code aside that "the scheduler queue is shallow in
practice" (`mesh.rs:10683`). **Single-stream → nothing to batch.**

**Result B — 16 concurrent scheduled streams (16 producers), 14,720 packets,
no inter-round sleep:**

| drain run-length | count |
|---|---:|
| 1 … 255     | 0 |
| **≥256**    | **1** |
| **longest single run (exact)** | **14,720 packets** |

With 16 independent producers feeding the one drain loop, the loop **never
found the queue empty** during the flood — it drained all 14,720 packets in a
**single continuous run**. This is the FairScheduler's actual purpose (fairness
across concurrent bulk streams), and it is **exactly the deep-backlog workload
where `sendmmsg` pays**: at `MAX_BATCH_SIZE = 64`, that one run collapses from
14,720 `send_to` syscalls to ⌈14720/64⌉ = **230 `sendmmsg` calls — a 64×
syscall reduction** (plus the matching collapse of send-loop wakeups).

### Verdict matrix

| workload | drain depth | batching? | evidence |
|---|---|---|---|
| unary `nrpc_qps` | — (off the send loop) | **No** | code trace (lead finding) |
| single scheduled stream | **1** | **No** | Result A (measured) |
| **N concurrent scheduled streams** | **very deep (one 14.7k run)** | **Yes — ~64×** | Result B (measured) |

So the original sendmmsg instinct was **right for a workload nobody had named**
— concurrent scheduled bulk transfers — and **wrong for the one that prompted
it** (unary QPS). Phase 1 is **revived but precisely scoped to the concurrent
scheduled-stream egress**; it must never be pitched against `nrpc_qps` again.

---

## Phase 1 — Batch drain in the scheduled-stream send loop

**Scope: the *concurrent* scheduled bulk-transfer egress only** — the lead finding
rules out unary, and Result A rules out the single-stream case (depth 1). The win
is real only when *multiple* scheduled streams feed the loop concurrently
(Result B: one 14,720-packet run). The shape is **conditional skip**: batch when
packets are already waiting, fall through to single `send_to` when they are not —
so a lone scheduled packet is unaffected and the win is captured whenever
concurrent producers back the loop up. The two details that make it good rather
than merely correct are the **depth probe** and **transport reuse**.

> **Correction (verified against HEAD):** the earlier draft proposed guarding on
> `total_queued() <= 1`. That does not work — `total_queued` is a monotonic
> cumulative enqueue counter, never decremented (`router.rs:201,214`;
> `tests/scheduled_stream.rs:8`). There is **no cheap current-depth signal today**.
> The conditional skip therefore needs one of the two options below; this is the
> design decision Phase 0's measurement informs.

```text
loop:
  let packet = scheduler.dequeue()? ;                  // first packet (today's path)
  if scheduler.current_depth() == 0 {                  // NEW atomic, see Option A — one Relaxed load
      socket.send_to(&packet.data, packet.dest).await; // unary fast path, ~unchanged cost
  } else {
      drain up to N more via dequeue() into batch       (N = MAX_BATCH_SIZE = 64)
      #[cfg(linux)]      batched_transport.send_batch(&batch, dest)   // one sendmmsg, reused transport
      #[cfg(not(linux))] for p in &batch { send_to(p).await }         // portable fallback
      // on EWOULDBLOCK from sendmmsg: fall back to async send_to(p).await for backpressure
  }
  if drained nothing -> select! { wait(), sleep(1ms) }
```

**The cheap-skip guard — two options, pick after Phase 0:**

- **Option A (preferred): add a real current-depth atomic.** A new
  `current_depth: AtomicU64` on `FairScheduler`: `fetch_add(1)` in `enqueue`
  (alongside the existing `total_queued` bump, `router.rs:201,214`) and
  `fetch_sub(1)` on every successful `pop` inside `dequeue` (`router.rs:275,308`,
  and the priority-queue pop at `:240`). The guard is then a single `Relaxed`
  load. Cost added to the system is one `fetch_sub` per dequeued packet — uniform
  and cheap. This is the only way the "near-free skip" claim is actually true.
- **Option B: accept a bounded probe drain (no new field).** Just
  `while batch.len() < N { match dequeue() { Some(p)=>push, None=>break } }`. But
  note the real cost: `dequeue()` allocates a `Vec<u64>` of stream keys **every
  call** (`router.rs:245`) — so at depth-1 the extra `dequeue()→None` *doubles* the
  per-packet allocation on the unary path, the c1 tripwire. Only acceptable if
  Phase 0 shows the unary path is not allocation-sensitive, or if the `Vec<u64>`
  alloc in `dequeue` is removed first (its own optimization, arguably overdue).
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

### Design constraint discovered during build: batch is **per-destination**

`PacketSender::send_batch` / `BatchedTransport::send_batch` take a **single**
`target: SocketAddr` for the whole call (`linux.rs:144`, all packets → one
`sockaddr`; also rejects IPv6 with `Unsupported`). But the scheduler interleaves
streams that may target **different peers**, so a drained batch can be mixed-dest.
The drain therefore **groups the drained packets by destination** and issues one
`send_batch` per peer. Per-stream ordering is preserved (packets to the same peer
keep dequeue order; the receiver demuxes by `stream_id`). When all concurrent
streams target one peer (the common bulk-fan-in case) there is exactly one group
and batches fill to 64.

### Phase 1 results — 2026-06-02 (macOS, portable path)

Implemented: `FairScheduler::current_depth` (enqueue `+1`, every successful pop
`-1`, `router.rs`); send loop reworked to depth-0 fast path + group-by-dest drain
(≤64) with a loop-owned reusable `groups` buffer; Linux `send_batch` per group
with async tail fallback behind `cfg(target_os="linux")`; portable per-packet
path elsewhere. Re-running the Phase 0 driver:

- **Single stream:** 4,600/4,600 still depth-1 → **100 % on the fast path**, zero
  behavior change (verified by the histogram, unchanged from before).
- **16 concurrent streams:** 14,720 packets → **230 flushes → exactly 64.0
  packets/flush = 64× fewer send syscalls** (one `sendmmsg` per flush on Linux).
- **Fairness suite green:** `test_fair_scheduler_*`,
  `round_robin_idx_advances_only_on_successful_pop` (#31 pin),
  `test_fair_scheduler_respects_stream_weight`, `*_priority`, `*_no_starvation`,
  `*_cleanup_called` — all pass. `scheduled_stream` routing test passes.
- **Grouping logic unit-tested:** `group_by_dest_partitions_preserving_per_dest_order`
  pins the multi-destination case the 16-stream integration test does *not* cover
  (it targets one peer → one group): interleaved packets to 3 peers partition into
  3 ordered groups, and the reuse-clear keeps slots while emptying inner vecs.
- **`current_depth` accounting:** `current_depth_tracks_live_backlog_and_returns_to_zero`
  — depth tracks enqueue−dequeue across priority + stream lanes, a rejected
  (queue-full) enqueue does not bump it, and a full drain returns it to 0.

### Linux validation — CI gate added (2026-06-02)

A named step in the `integration-tests` job (`ubuntu-latest`) now **compiles and
runs** the `cfg(target_os="linux")` path that the macOS dev host cannot:

```yaml
- name: Scheduled-stream batched drain (Linux sendmmsg path)
  run: cargo nextest run --no-fail-fast --features net --no-capture
       --test scheduled_stream --test send_drain_depth
```

(Added because *none* of `scheduled_stream` / `send_drain_depth` /
`transfer_concurrency` were referenced anywhere in `ci.yml` — the batch path had
no CI coverage at all.) A cross `cargo check --target x86_64-unknown-linux-gnu`
from macOS was tried first but is blocked by a `-sys` dep needing
`x86_64-linux-gnu-gcc`, so CI is the only place the cfg block compiles.

**What the gate proves:** the Linux block **compiles**, the batch drain **runs**
on real Linux sockets without panic/error under a 16-stream flood, routing holds,
and the packets/syscall collapse prints in the log.

#### CI result — 2026-06-02 (Linux / ubuntu-latest) ✅ PASSED

Both tests green; the `cfg(linux)` `sendmmsg` path **compiled and ran**:

- **Single stream:** 4,575 / 4,587 runs length-1 (99.7 %) — depth ≈ 1, identical
  conclusion to the macOS run. Single stream still has nothing to batch.
- **16 concurrent streams:** 14,729 packets → **238 flushes → 61.9 packets per
  `sendmmsg` ≈ 62×** fewer send syscalls — at the theoretical 64 ceiling. The
  collapse is real on **actual syscalls**, not just the flush-count proxy the
  macOS run measured.
- **Platform difference (expected):** macOS drained the flood as *one* continuous
  14,720-packet run; Linux broke into **5 runs** (longest 3,745). Real `sendmmsg`
  drains the queue fast enough that the single-consumer loop occasionally catches
  up and empties between bursts — yet batches still fill to ~62, so the syscall
  win is undiminished. (Confirms the win comes from *concurrency-driven backlog*,
  and that a faster consumer shortens runs without shrinking batches.)

So the headline is validated on real hardware: **concurrent scheduled bulk streams
→ ~62× fewer send syscalls** via the batched group-by-dest drain.

### Delivery integrity — closed (`scheduled_stream_integrity`, 2026-06-02)

The earlier gap ("a silently mis-delivering sendmmsg would pass `send_drain_depth`
because it's `FireAndForget` with a `send_to` fallback") is now covered by a
**reliable** end-to-end integrity test, wired into CI:

- `tests/scheduled_stream_integrity.rs` (`#![cfg(feature = "dataforts")]`):
  16 blobs fetched concurrently from one holder over **reliable blob transfer**,
  whose holder-side data rides `scheduled = true` streams
  (`dataforts/blob/transfer.rs:484`) → the holder's send loop backs up into the
  **batch path**. Every fetched blob is asserted **byte-for-byte** against its
  source, and `flushes > 0` asserts the batch path was actually used.
- **Local result (macOS, portable drain):** `16/16 blobs byte-for-byte`,
  286 packets in 8 flushes — integrity holds and the batch path is exercised even
  without real `sendmmsg`.
- **On Linux CI** the same test runs the real `sendmmsg` + tail-fallback **under
  content verification**, so a mis-delivering or byte-corrupting batch send now
  fails the build. Added as the `Scheduled-stream batched-drain integrity` step
  (`--features dataforts`).
- Kept light (128 KiB blobs, 512 KiB buffers) so it binds on macOS — unlike
  `transfer_concurrency` (4 MiB / 8 MiB, `os error 55` on macOS), which stays out
  of CI.

### Partial-send / `EWOULDBLOCK` tail fallback — covered (2026-06-02)

`scheduled_stream_integrity` now runs a **second phase** with a **16 KiB holder
send buffer**. Under the 12-stream concurrent flood, a ~90 KiB `send_batch` group
cannot fit, so on Linux `sendmmsg` returns a partial count (or `EWOULDBLOCK`) and
the drain's `for d in &data[sent..] { send_to … }` tail-fallback executes — and the
**reliable byte-for-byte assertion still holds**, so a broken tail (wrong slice,
dropped tail, mis-order) fails the build. Local (macOS, portable path): both
phases green — `normal` 16/16, `tiny-send-buf` 12/12 byte-for-byte. The CI step is
unchanged (it already runs `--test scheduled_stream_integrity`); the new phase
rides along.

> Caveat: on macOS phase 2 takes the portable `send_to` path (no `sendmmsg`), so it
> proves delivery-under-squeeze but not the sendmmsg tail itself — that assertion
> lands on Linux CI, where the cfg block is live.

### Gaps still open

*(none material — all paths exercised; remaining work is the deferred follow-ups
below: reusable `BatchedTransport` and wakeup-collapse latency measurement.
Stale-slot pruning is now done — see follow-ups.)*

### Follow-ups (deferred)

- **Loop-owned reusable `BatchedTransport`** instead of `PacketSender::send_batch`
  (which rebuilds 3 × `Vec::with_capacity(64)` per call, `transport.rs:492-496`).
  The single-consumer send loop can own one `mut BatchedTransport` — no lock. Skipped
  in v1 to reuse the already-tested wrapper and shrink the untested Linux surface.
- ~~**Prune stale dest slots** in `groups`~~ — **done** (`reset_dest_groups`):
  the dest-slot set was growing monotonically (one slot per peer ever seen) under
  peer churn, inflating memory and the linear `group_by_dest` scan (flagged in
  review). Reset now keeps slot reuse for the hot path but drops the whole set
  once it exceeds `MAX_DRAIN`, so it stays at `cap + 1` worst case. Pinned by
  `reset_dest_groups_stays_bounded_under_peer_churn`.
- **Wakeup collapse** (the 51 % `NtWaitForAlertByThreadId` bucket) is a *latency*
  win the batch drain also enables but this plan does not measure.

### Cross-platform

`sendmmsg` is Linux-only; macOS has no direct equivalent and falls back to the
portable `send_to` loop — i.e. the substrate sends faster on Linux than macOS
under saturation. For the deployment target (Linux) this is acceptable; state it
explicitly wherever the number is quoted so the macOS gap is not a surprise.

---

## Phase 2 — Saturated one-way send bench (the honest home)

Give the 10–20× sendmmsg claim a workload that actually exercises it, separate
from the latency bench:

- New bench `nrpc_send_throughput` (or a group in `nrpc_qps.rs`): blast bulk
  payloads over **N concurrent scheduled streams** (`config.scheduled = true`, the
  only path the send loop drains; N ≥ 8 per Result B) fire-and-forget, measure
  packets/sec drained by the send loop. This is where the loop sustains a deep run
  and `sendmmsg` shows its win — a *single* stream stays at depth 1 (Result A) and a
  unary fire-and-forget bypasses the loop entirely (lead finding). The
  `tests/send_drain_depth.rs` driver already demonstrates the backlog and can seed
  this bench.
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

- **Don't claim a `nrpc_qps` win from send batching — it is not on that path.**
  The lead finding is the guard: unary bypasses the loop, so any `nrpc_qps` delta
  from Phase 1 is noise. Report the scheduled-stream bench (Phase 2) for the
  sendmmsg number, never `nrpc_qps`.
- **Don't regress the scheduled-stream single-packet path.** The Phase 1 drain must
  keep the depth-≤1 path identical to today's single `send_to`. Streaming benches
  (`nrpc_streaming.rs`) and `nrpc_qps c128/16KiB` remain the standing tripwires for
  not collaterally regressing anything.
- **tokio readiness bypass (Phase 1, Linux).** Raw-fd `sendmmsg` sidesteps tokio
  writability; `EWOULDBLOCK` must fall back to async `send_to().await`, never spin.
- **Fairness property (Phase 3).** Multi-send-loop without an atomic cursor
  advance silently breaks the scheduler's fairness guarantee — fairness tests are
  the gate, and they flake rather than fail loudly, so this needs deliberate
  verification, not a green CI by luck.
- **Bench honesty.** As in the companion plan, the shared single runtime for
  client+server colors absolute numbers; note it wherever quoted.

## Open questions for Phase 0 to answer

- Under a **saturated scheduled bulk stream**, does the send-loop drain run-length
  actually climb toward 64 (where sendmmsg pays), or does per-stream credit/window
  flow-control drip-feed it ≈1 packet at a time? (If the latter, there is **no live
  workload** that benefits and Phase 1 should be deferred.)
- Is there any path other than `scheduled=true` streams that should be routed
  through the scheduler egress (and thus benefit from batching), or is the direct
  `publish_to_peer` → `socket.send_to` the deliberate design for everything unary?
