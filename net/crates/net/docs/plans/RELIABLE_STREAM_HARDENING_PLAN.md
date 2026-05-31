# Reliable-stream hardening

**Status:** New. Follows `STREAM_RETRANSMIT_PLAN.md` (retransmit now wired). Wiring retransmit surfaced a cluster of reliability deficiencies; this plan addresses them, ordered correctness → observability → efficiency. Cross-cutting — touches every reliable stream (nRPC streaming, blob transfer, …).

## Deficiencies (observed)

| ID | Deficiency | Severity |
|----|-----------|----------|
| **H-1** | **Fixed retransmit window (`max_pending`=32) vs the tx-credit window.** If a stream's tx-window admits >32 packets in flight before the receiver's grants catch up, the oldest unacked packets are *evicted* from the retransmit window (`untracked_evictions`) and become unrecoverable. Today only avoided by hand-coupling the transfer window to 32 — a latent footgun for any larger-window reliable stream. | **High (silent loss)** |
| **H-2** | **`untracked_evictions` is unobservable** — a counter with no metric/log, so silent loss is invisible in production. | High (observability) |
| **H-3** | **No hard-failure signal.** When a packet exhausts `max_retries` (3), `get_timed_out` drops it from the window — the gap becomes permanent and the *caller* only finds out via its own 30 s timeout. No stream reset/abort is surfaced. | Medium |
| **H-4** | **NACKs only fire on consumption progress.** The receiver emits a NACK piggybacked on a grant, but a gap halts consumption → no grant → no NACK. Recovery then waits for the sender's RTO (D-4). A receiver-side proactive NACK on a persistent gap would recover faster. | Medium |
| **H-5** | **Fixed 50 ms RTO, no RTT estimation / backoff.** Crude: spurious resends on a slow WAN, sluggish on a fast link. | Low–Med |
| **H-6** | **No loss-responsive window.** The tx-window is static; under loss the sender keeps blasting the full window. No AIMD / slow-start. | Low–Med |
| **H-7** | **`CloseBehavior::DrainThenClose` is not honored** by `close_stream` (it removes state immediately). `serve_chunk` hand-rolls an ack-wait close; this should be a first-class stream primitive. | Low |
| **H-8** | **In-order delivery is not a guarantee.** `on_receive` accepts out-of-order sequences; the substrate delivers events in *arrival* order. The blob-transfer engine reorders by seq itself — but other reliable-stream consumers (nRPC streaming reassembly?) may assume in-order delivery and silently corrupt under reordering/retransmit. Needs investigation, then either an in-order delivery buffer or a documented contract. | **Investigate (potential High)** |

## Stages (in-scope now)

### H-1 — Auto-size the retransmit window to the tx-window; stop pre-reserving
- `ReliableStream::pending` is created with `VecDeque::new()` (grow-on-demand), NOT `with_capacity(max_pending)` — so the retransmit window can be generous without per-stream up-front memory; the queue only grows to the actual in-flight count, itself bounded by the tx-window bytes.
- `create_reliability_mode(reliable, max_pending)`; `StreamState::new_full_with_epoch` derives `max_pending` from `tx_window` (≈ `tx_window / MIN_TRACKED_PACKET_BYTES`, floored at `DEFAULT_MAX_PENDING`, capped). So a stream can never have more unacked packets in flight than it can retransmit — the H-1 invariant holds for *any* window, removing the footgun and letting the transfer use a larger window again if desired.
- Test: a large-window stream under loss recovers (no `untracked_evictions`).

### H-2 — Surface `untracked_evictions`
- A rate-limited `warn!` + a metric when a stream evicts an unacked packet (the silent-loss signal). With H-1 this should never fire for well-configured streams; if it does, it means the window/packet-size assumption was violated and data was lost.

### H-3 — Hard-failure signal on retransmit give-up
- When a descriptor exhausts `max_retries`, mark the stream failed and send a `SUBPROTOCOL_STREAM_RESET` to the peer so the receiver fails its pending read promptly (and with a distinct error) instead of stalling to the caller's timeout. The blob-transfer engine maps a reset to `BlobError`.

## Deferred (documented, not now)
- **H-4** receiver-side proactive NACK timer.
- **H-5** adaptive RTO (RTT estimate + Karn + exponential backoff).
- **H-6** loss-responsive window (slow-start / AIMD).
- **H-7** `DrainThenClose` graceful-close primitive (generalize `serve_chunk`'s ack-wait).
- **H-8** in-order-delivery investigation + fix — its own analysis; high potential severity but needs scoping first.

## Order
H-1 → H-2 (they pair: H-1 prevents the loss, H-2 proves it). → H-3. Re-run the retransmit + transfer/dir/fairness + nrpc suites after each. Then revisit deferred items by measured need.

## Non-goals
- Not reimplementing TCP. Congestion control (H-6) and adaptive RTO (H-5) are bounded, optional refinements, not a full transport rewrite.
- The `mod.rs` legacy send path stays as-is.
