# nRPC receive-loop batching (recvmmsg)

## Status

**Implemented opt-in (default OFF); flipping the default is parked pending the
c128 measurement.**

Gated at two levels:

- **Build feature `batched-ingress`** (off by default) decides whether the
  batching path — the mesh receive-loop integration and the recv instrument —
  is *compiled in at all*. Default builds contain only the per-packet path.
- **Runtime flag `MeshNodeConfig::batched_ingress`** (present only under that
  feature, default `false`, no-op off Linux) decides whether it's *enabled*.

(The shared `BatchedPacketReceiver` / `recv_batch` machinery stays compiled
unconditionally — it predates this work and is also used by `NetAdapter`'s
ingress; the feature gates the mesh-node integration + instrument, not that
shared wrapper.)

Stages 1–5 of this plan have shipped under the `batched-ingress` feature:

1. Recv-side instrument (`arm_recv_drain_histo` / `recv_drain_histo_snapshot` /
   `recv_drain_max` / `recv_batch_stats`, `#[doc(hidden)]` in `transport.rs`).
2. Gap-fix #1: the batched receiver hands a whole recvmmsg batch over the
   channel per syscall instead of one packet per `blocking_send`.
3. The mesh receive loop (`MeshNode::spawn_receive_loop`) uses
   `BatchedPacketReceiver` when `batched_ingress` is on, via a local
   `IngressReceiver` enum so the dispatch loop is written once.
4. `tests/batched_ingress_integrity.rs` — byte-for-byte delivery through the
   batched path + (Linux) `recv_batch_stats().syscalls > 0`.
5. Linux CI step exercising the recvmmsg path.

What remains is **the decision**, not the code: run the c128 throughput +
unary-latency measurement (below), and if it justifies the cross-thread
channel-hop tax, flip `batched_ingress` to default-on (or enable it per
deployment). Gap-fix #2 (kernel-blocking idle instead of the 1 ms poll) is
still deferred — measure first. The rest of this document is the original
design rationale, retained for that decision.

NOTE: the Linux recvmmsg path is not compilable on the Windows dev host used
to implement these stages (cross toolchain unavailable, WSL broken), so the
batched arm is validated by reasoning + the Linux CI gate; the off path,
config plumbing, instrument, and integrity harness build and pass locally.

The original framing of this plan ("build a recvmmsg wrapper symmetric to the
sendmmsg work") is out of date. While auditing the code for this plan we found
that the Linux `recvmmsg` wrapper **and** a batched receiver task already
shipped:

- `BatchedTransport::recv_batch` / `recv_batch_blocking` — the `recvmmsg`
  syscall wrapper (`src/adapter/net/linux.rs:316` and `:422`).
- `BatchedPacketReceiver` — a dedicated-OS-thread + bounded-mpsc receiver that
  owns the `!Send` `BatchedTransport` and hands decoded packets to async code
  (`src/adapter/net/transport.rs:303`).
- `NetAdapter::spawn_receiver` — **already** uses `BatchedPacketReceiver` on
  Linux, with a per-packet `recv_buf_from` fallback off-Linux
  (`src/adapter/net/mod.rs:817` Linux / `:856` non-Linux).

So the wrapper is done and one adapter already benefits. **The remaining gap is
narrow and specific:** the substrate's primary ingress path — the `MeshNode`
steady-state receive loop in `MeshNode::spawn_receive_loop`
(`src/adapter/net/mesh.rs:3144`, the loop body at `:3214`) — still calls the
per-packet `PacketReceiver::recv()` and dispatches one packet at a time. nRPC,
blob transfer, RedEX replication, and every channel subscription ride this
loop. It is the loop a production engineer will profile, and it does not batch.

This plan is now: **(1) decide, via the c128 measurement, whether the MeshNode
loop should adopt the existing batched receiver; (2) if yes, close the two
efficiency gaps in the current batched-receiver design before wiring it in;
(3) add recv-side observability symmetric to the send side; (4) test and gate.**
That is a fraction of the original ~500–800 LoC estimate.

## Why this might matter

The send-loop batching that shipped (`NRPC_SEND_LOOP_BATCHING_PLAN.md`)
collapses outbound syscalls on the scheduled-stream drain — measured 62×
reduction at c16. The `MeshNode` ingress path was not touched: it still calls
`socket.recv_buf_from(...).await` one datagram at a time per loop iteration
(`mesh.rs:3219` → `PacketReceiver::recv`, `transport.rs:277`).

After the send-side fix the substrate can dispatch packets faster than a
per-packet ingress loop can drain them, which under sustained high-throughput
workloads makes the `MeshNode` receive loop the new bottleneck. Egress is
bursty (responses to requests); ingress is sustained (peers sending data
continuously), so the receive-side scaling property matters more than the
send-side one for most real workloads.

The asymmetry is also visible from reading the code: an engineer who read the
sendmmsg PR and then opens `spawn_receive_loop` sees per-packet `recv`. Having
the symmetric answer either shipped or concretely scoped is what makes the
performance story complete rather than half-done. (Note that `NetAdapter`
already batches ingress, so the honest answer today is "we batch ingress on the
single-session adapter; the mesh path is measured-and-pending," not "we don't
batch ingress at all.")

## When to revisit and implement

Implement if any of:

- The post-sendmmsg `nrpc_qps` c128 throughput benchmark shows the **MeshNode**
  receive side as the dominant bottleneck — a `perf record` of a c128 run shows
  time in `recvmmsg`/`recv_buf_from` and kernel context switches dominating
  over packet processing (decrypt, capability folds, scheduler atomics,
  DashMap).
- A specific customer/evaluation workload (Hermes integration testing,
  hyperscaler review, demo) shows throughput constrained by mesh ingress at a
  scale the customer cares about.
- A seed-conversation engineer raises receive-side scaling and "we shipped
  send-side but the mesh ingress is still per-packet" weakens the conversation.

Do not implement preemptively if:

- The c128 measurement shows mesh ingress is not the bottleneck.
- The per-packet→channel-hop latency tax (see "Critical risk" below) would
  regress nRPC unary p50/p99 at the concurrencies customers actually run, and
  the throughput win doesn't justify it.
- Implementation time would push Hermes integration past the seed window.

## Current state (what exists, with line references)

| Piece | Status | Location |
| --- | --- | --- |
| `recvmmsg` wrapper (`MSG_DONTWAIT`, returns `Vec<(Bytes, SocketAddr)>`) | **Exists** | `linux.rs:316` `recv_batch`; `:422` `recv_batch_blocking` |
| `BatchedTransport` recv buffer pool (reused slots, no-memset `set_len`) | **Exists** | `linux.rs:316–418` |
| Thread+channel receiver (sidesteps `!Send` `BatchedTransport`) | **Exists** | `transport.rs:303` `BatchedPacketReceiver` |
| `BatchedTransport: Send` guard | **Exists** (added with sendmmsg reuse work) | `linux.rs` `unsafe impl Send` + `const _` |
| Single-session adapter uses batched ingress | **Exists** | `mod.rs:817` `NetAdapter::spawn_receiver` (Linux) |
| **MeshNode ingress uses batched receive** | **MISSING** | `mesh.rs:3214` still `PacketReceiver::recv()` per packet |
| Recv-side observability (`recv_drain_*`, `recv_batch_stats`) | **MISSING** | — (send-side analogues in `router.rs`, now `#[doc(hidden)]`) |
| Linux CI exercising mesh batched ingress | **MISSING** | extend the existing sendmmsg step in `.github/workflows/ci.yml` |

## Design

`BatchedTransport` holds raw `libc::iovec`/`mmsghdr` pointers and is therefore
`!Send` (the same property the send side hit). It carries an `unsafe impl Send`
for the *single-owner, never-across-threads-concurrently* case, but the recv
path historically chose the **dedicated-OS-thread + channel** shape instead of
holding the transport in the async task. That is the shape to reuse — do **not**
re-derive the inline-`recvmmsg`-in-the-async-loop pseudocode from the original
draft of this plan; it cannot hold a `!Send` transport across `.await` and the
thread+channel form already solves it.

### Integration: mirror `NetAdapter::spawn_receiver` in the mesh loop

Replace the `PacketReceiver` in `MeshNode::spawn_receive_loop` (`mesh.rs:3215`)
with the same `cfg`-split `NetAdapter::spawn_receiver` already uses:

```rust
// mesh.rs, inside spawn_receive_loop's tokio::spawn body
#[cfg(target_os = "linux")]
let mut receiver = transport::BatchedPacketReceiver::new(socket);
#[cfg(not(target_os = "linux"))]
let mut receiver = PacketReceiver::new(socket);

while !shutdown.load(Ordering::Acquire) {
    tokio::select! {
        result = receiver.recv() => {
            match result {
                Ok((data, source)) => Self::dispatch_packet(data, source, &ctx),
                // BatchedPacketReceiver surfaces a dead recv thread as
                // ConnectionReset — break, mirroring mod.rs:835.
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
                Err(e) => {
                    if !shutdown.load(Ordering::Acquire) {
                        tracing::warn!(error = %e, "mesh receive error");
                    }
                }
            }
        }
        _ = shutdown_notify.notified() => break,
    }
}
```

The dispatch path (`dispatch_packet`, `mesh.rs:3245`) is unchanged: protocol
decode, partition filter, keep-alive recognition, session decrypt, capability
checks, channel auth, handler invocation all still happen one packet at a time.
Batching only changes how packets cross the kernel→userspace boundary and how
they reach the async task.

Both `BatchedPacketReceiver` and `PacketReceiver` already expose
`async fn recv(&mut self) -> io::Result<(Bytes, SocketAddr)>`, so the loop body
is identical across the `cfg` split — only the constructor differs.

### Two efficiency gaps to close before this is worth shipping

The existing `BatchedPacketReceiver` was built for `NetAdapter` and has two
properties that blunt the win on the high-throughput mesh path. Both are real,
both are visible at `transport.rs:367–395`:

1. **Per-packet channel hop.** The recv thread does
   `tx.blocking_send(packet)` once *per packet* (`transport.rs:381–385`), so a
   64-packet `recvmmsg` becomes 64 channel sends + 64 cross-thread handoffs.
   That re-imposes per-packet overhead the syscall batching just removed.
   **Fix:** change the channel item type to the whole batch
   (`mpsc::channel<Vec<(Bytes, SocketAddr)>>` or a `SmallVec`), send it in one
   `blocking_send`, and have `recv()` drain an internal `VecDeque` between
   channel reads. Collapses 64 channel ops → 1 per syscall while keeping
   `recv()`'s one-packet-at-a-time signature, so neither dispatch loop changes.

2. **Idle 1 ms busy-poll.** When `recv_batch` returns empty (WouldBlock), the
   thread `sleep(1ms)` and re-polls (`transport.rs:373–379`). At idle that is a
   1 kHz wakeup burning a core fraction and adding up to 1 ms latency to the
   first packet after an idle gap — a latency tax exactly where nRPC unary is
   most sensitive. **Fix (optional, measure first):** block on the fd with
   `poll()`/`epoll` (or a short `recv_batch_blocking` with a self-pipe/eventfd
   for shutdown) so the thread sleeps in the kernel until data or shutdown,
   then drains with `MSG_DONTWAIT`. Keep the shutdown-responsiveness the 1 ms
   poll currently provides — the existing comment at `transport.rs:336–350`
   explains why `SO_RCVTIMEO` on the shared `Arc<UdpSocket>` is **not** an
   option (it leaks to the sync handshake recv consumers).

### Critical risk: unconditional channel-hop latency tax

The send side kept a **fast path** for `current_depth() == 0` so depth-≤1
traffic never paid batching overhead. The thread+channel receiver has no such
escape hatch — *every* inbound packet now takes a cross-thread hop, even a lone
nRPC unary request at c1. This can **regress** unary p50/p99 latency while
improving bulk throughput. This is very likely why the mesh loop was never
switched, and it is the single most important thing the c128 measurement must
check: **measure latency at low concurrency, not just throughput at c128.**
If unary latency regresses materially, options are (a) ship batched ingress
only for the bulk/transfer-heavy deployments via config, (b) keep the inline
`PacketReceiver` for the mesh path and accept that ingress batching lives only
on `NetAdapter`, or (c) invest in an inline `recvmmsg` that drains-then-yields
without a thread hop (larger, needs a `Send`-safe drain or a `LocalSet`).

### Ordering preservation

`recvmmsg` returns packets in kernel arrival order, and `recv_batch` preserves
that order in its result `Vec` (`linux.rs:404–415`). The batched channel
(gap-fix #1) must preserve it too: push the batch in order, drain front-to-back.
Single-peer traffic must dispatch in the same order it would under per-packet
receive — verified by test below. The substrate already tolerates UDP
reordering and interleaved multi-peer traffic, so batching introduces no new
ordering semantics, only a pinned invariant.

### Cross-platform fallback

`cfg(target_os = "linux")` selects `BatchedPacketReceiver`; everything else
keeps `PacketReceiver` (the existing per-packet `recv_buf_from` path). This is
exactly the split `NetAdapter::spawn_receiver` already uses (`mod.rs:817` vs
`:856`). macOS/Windows development is unaffected; production Linux gets the
optimization. No runtime platform detection.

### Backpressure

Unchanged at the substrate level. The recv thread drains the kernel buffer into
the bounded channel (capacity 1024 today, `transport.rs:329`). If dispatch
can't keep up, the channel fills, the thread's `blocking_send` parks, the kernel
socket buffer fills, and the network layer's existing flow control (NACK,
congestion window, retransmission) takes over — identical to today. Note the
channel adds one more bounded buffer in front of the kernel buffer; size it
deliberately (with gap-fix #1 the item is a *batch*, so capacity 1024 batches ≫
1024 packets — reconsider the number).

## Implementation scope

Files to touch:

- `src/adapter/net/mesh.rs` — swap the receiver in `spawn_receive_loop`
  (`:3215`) for the `cfg`-split batched/non-batched pair. ~15 LoC.
- `src/adapter/net/transport.rs` — gap-fix #1 (batch the channel item),
  optional gap-fix #2 (kernel-blocking idle). ~40–120 LoC.
- Recv-side observability hooks (see below). ~60 LoC.
- `.github/workflows/ci.yml` — extend the existing Linux batched-drain step.
  ~5 LoC.
- Tests — `tests/recv_drain_depth.rs` and a mesh integrity/ordering test.
  ~250 LoC.

Approximate total: **~350–450 LoC**, most of it tests. Far smaller than the
original estimate because the wrapper, the thread+channel receiver, the `Send`
guard, and the cross-platform split already exist.

Time estimate: ~1 day integration + gap-fixes, ~1 day tests, ~0.5 day
benchmarks/docs. ~2.5 days, contingent on the measurement saying "go."

## Observability

Mirror the send-side instrument, which lives in `router.rs` and is now
`#[doc(hidden)]` (re-exported only for in-repo tests):

| Send side (`router.rs`) | Recv-side analogue | Meaning |
| --- | --- | --- |
| `arm_send_drain_histo()` | `arm_recv_drain_histo()` | latch the histogram on before start |
| `send_drain_histo_snapshot()` | `recv_drain_histo_snapshot()` | log2 buckets of per-`recvmmsg` batch size |
| `send_drain_max()` | `recv_drain_max()` | largest single batch observed |
| `send_batch_stats() -> (flushes, packets)` | `recv_batch_stats() -> (syscalls, packets)` | `packets / syscalls` = realized recv syscall-collapse factor |

Same discipline as the send side: process-global atomics, armed once before the
loop starts, near-zero cost when unarmed, `#[doc(hidden)]` so they stay off the
public surface, read by tests via deltas. Place them next to the recv thread
(`transport.rs`) since that is where the `recvmmsg` count lives.

## Tests

Symmetric to the send-side suite (`tests/send_drain_depth.rs`,
`tests/scheduled_stream_integrity.rs`) plus recv-specific concerns:

- **Syscall-collapse measurement** (`tests/recv_drain_depth.rs`): flood a node
  with concurrent inbound streams; assert `recv_batch_stats()` shows
  `packets / syscalls` ≫ 1 under load (the recv analogue of `send_drain_depth`).
- **Throughput**: `nrpc_qps` c128 (32B + larger payloads) before/after,
  demonstrating mesh ingress batching moves the metric — the c128 number the
  send-side work did not directly measure.
- **Latency guard (the critical one)**: nRPC unary p50/p99 at c1/c4 before/after.
  Must NOT regress materially — this is the channel-hop tax check.
- **Ordering preservation**: single-peer traffic dispatched in arrival order
  whether it arrived as one batch or many.
- **Multi-peer mixed batch**: packets from different peers in one `recvmmsg`
  dispatched without cross-contamination (right `source` per packet).
- **Integrity under buffer pressure**: reuse the `scheduled_stream_integrity.rs`
  shape (tiny socket buffers, concurrent reliable blob fetches) on the *receive*
  side; assert byte-for-byte delivery so batched ingress + protocol-level loss
  recovery (NACK/retransmit) still holds.
- **Shutdown/exit**: dead recv thread surfaces `ConnectionReset` and the mesh
  loop breaks promptly (mirror `mod.rs:835`); `Drop` joins the thread
  (`transport.rs:448`) without hanging.
- **Cross-platform fallback**: on non-Linux the `PacketReceiver` path is taken
  and behavior is observably identical.

## Risks and open questions

- **Channel-hop latency tax (critical).** See the Design section — measure unary
  latency at low concurrency, not just c128 throughput. This gates the whole
  decision.
- **Kernel version / recvmmsg semantics.** The wrapper uses `MSG_DONTWAIT` and
  no timeout pointer (`linux.rs:390–392`), avoiding the pre-4.18 timeout-handling
  quirks. Document the minimum supported kernel and verify on target deploy
  environments.
- **Buffer/channel sizing under bursty load.** Fixed pools (64 × 8 KiB recv
  slots ≈ 512 KiB per receiver, plus the mpsc channel) are fine for steady
  state; revisit if bursty workloads show channel-full stalls. Ship fixed, tune
  on observed production behavior.
- **Interaction with flow control.** Reading more packets per syscall shifts
  *when* the substrate observes inbound packets. The substrate operates above
  per-packet timing (windowed flow control), so most likely no change — verify
  with the integrity-under-pressure test.
- **Two ingress paths drift.** `NetAdapter` and `MeshNode` would both batch but
  via the same `BatchedPacketReceiver`; keep them sharing the type so a fix to
  one helps both. Don't fork the receiver.
- **Measurement before commitment.** The work earns its existence only if mesh
  ingress is actually the bottleneck. The measurement is cheap; the integration
  is bounded but real.

## Decision point

Before implementing:

1. Run `nrpc_qps/c128/32B` and larger-payload variants on the current
   sendmmsg-batched substrate (mesh path, `MeshNode`).
2. `perf record` a c128 run; confirm whether `recvmmsg`/`recv_buf_from` +
   context switches dominate over dispatch (decrypt, folds, scheduler atomics,
   DashMap).
3. **Also** measure unary p50/p99 at c1/c4 to quantify the channel-hop tax the
   batched receiver would add.
4. If recv-side syscalls dominate **and** the latency tax is acceptable:
   implement (integrate + gap-fixes + instrument + tests + CI).
5. If recv-side syscalls dominate **but** the latency tax regresses unary:
   ship batched ingress behind config for bulk-heavy deployments, or leave the
   mesh path per-packet and document that ingress batching lives on
   `NetAdapter`.
6. If recv-side syscalls don't dominate: park. The substrate is genuinely done;
   next work is documentation.
7. If the profile shows something unexpected (DashMap contention, scheduler
   atomics, capability fold lookups in the hot path): different fix needed; this
   plan doesn't apply.

Measurement-first, matching the discipline the send-side work followed:
identify the bottleneck by measurement, scope the bounded fix, ship with
verification. Don't optimize without evidence — especially here, where half the
substrate already exists and the remaining risk is a latency regression, not a
missing capability.

## Relationship to other work

- **NRPC_SEND_LOOP_BATCHING_PLAN.md** (shipped): the symmetric send-side
  optimization. This plan reuses its observability pattern, its
  `cfg(target_os = "linux")` split, and the `BatchedTransport` + `unsafe impl
  Send` machinery that work added. Key difference: the send loop kept a
  depth-≤1 fast path to avoid a latency tax; the recv thread+channel design has
  no such escape hatch, which is the central open risk here.
- **FairScheduler transport work**: receive batching doesn't touch fairness —
  fairness is enforced on the send side where streams compete for outbound
  bandwidth (`router.rs` `FairScheduler`). Ingress just drains the socket.
- **Blob transfer / RedEX replication**: sustained-arrival workloads benefit
  from receive batching more than bursty request/response nRPC. The
  integrity-under-pressure test should use the blob-transfer path
  (`dataforts/blob/transfer.rs`), as the send-side integrity test does.
- **Capability fold query path**: single-packet capability queries see no
  batching benefit and must not regress (the latency guard test covers this).

## Origin

Identified during the post-sendmmsg conversation about ingress-side scaling.
The send-side optimization closed half of the syscall asymmetry production
engineers probe. While scoping the receive half we found the `recvmmsg` wrapper
and a batched receiver had already shipped and were wired into `NetAdapter`, so
the honest remaining gap is narrow: the `MeshNode` steady-state loop, plus two
efficiency rough edges in the existing batched receiver, plus a latency-tax
question that only measurement can answer. The plan captures the concrete design
so the implementation decision stays open without losing the thinking — and so
the next person doesn't re-discover that the wrapper already exists.
