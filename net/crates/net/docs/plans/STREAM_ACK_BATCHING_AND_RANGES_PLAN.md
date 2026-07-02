# Stream ACKs — batched emission + range-bound (SACK-range) ACKs

**Status:** PLANNED (drafted 2026-07-02). Two independent phases: Phase 1
(batched ACK/grant/NACK emission — wire-compatible, no format change) and
Phase 2 (range-bound ACKs — new subprotocol, capability-gated). Phase 1 is
prerequisite-free and its per-peer grouping is the emission structure
Phase 2 rides on.

## Background — how ACKs work today (verified 2026-07-02)

There is no standalone ACK packet. The receiver's cumulative ack
(`next_expected`) piggybacks as the `ack_seq` field of the `StreamWindow`
credit grant (`subprotocol/stream_window.rs:70-83`; 24-byte payload:
`stream_id`, `total_consumed`, `ack_seq`; `SUBPROTOCOL_STREAM_WINDOW =
0x0B00`, rides `CONTROL_STREAM_ID`).

- **Cadence:** every accepted data packet enqueues a `PendingStreamGrant`
  keyed `(session_id, stream_id)` with latest-wins overwrite
  (`mesh.rs::process_local_packet`, ~5371). The drainer
  (`spawn_stream_grant_drainer_loop`, ~5645) wakes on notify or every
  `STREAM_GRANT_DRAIN_INTERVAL` (1 ms) and emits **one wire packet per
  (peer, stream)** — plus a *second* packet per stream if a `StreamNack`
  piggybacks.
- **Sender side:** on `StreamWindow` receipt (`mesh.rs` ~4778) →
  `apply_authoritative_grant` + `ReliableStream::on_ack(ack_seq)`
  (`reliability.rs:731-801`): pop-front prune of the retransmit window,
  Karn-compliant RTT sample, cwnd growth (H-6/H-9).
- **Loss signaling:** `StreamNack { stream_id, next_expected,
  missing_bitmap: u64 }` — a selective *negative* ack with a hard horizon
  of 64 sequences past the head gap. Emitted piggyback in the drainer and
  proactively every `RETRANSMIT_TICK` (25 ms) via `collect_gap_nacks`
  (H-4). RTO loop backstops tail loss (D-4).

## The gap

**A. No batched emission.** Coalescing exists per-stream (latest-wins
map), but emission is one AEAD encrypt + one `sendto` per (peer, stream)
per drain cycle, ×2 when a NACK rides along. With S active streams to a
peer that is up to 2·S control packets per millisecond. The decode side
**already loops multi-event frames** for both `StreamWindow` and
`StreamNack` (`mesh.rs` ~4780 / ~4827 — "the codec supports multi-event
frames"), and `RxCreditState::on_bytes_consumed`'s doc explicitly
anticipates: *"a future enhancement can batch grants without changing the
wire format"* (`session.rs` ~1125). Only the encode side is missing.

**B. No positive SACK ranges.** The only positive ack is the single
cumulative `ack_seq`; out-of-order receipt is expressed only negatively
(64-bit NACK bitmap). Two concrete failure modes:

1. **RTO flood.** Packets the receiver already holds (above the head gap)
   cannot be pruned from the sender's retransmit window (`pending`, up to
   `MAX_RETRANSMIT_WINDOW = 16_384` entries). When the head-gap packet's
   RTO fires, `get_timed_out` (`reliability.rs:683-721`) retransmits
   **every** timed-out packet — including everything the receiver already
   has — and collapses cwnd to `MIN_CWND`. One lost packet on a bulk
   stream ⇒ potentially thousands of spurious retransmits at the exact
   moment the link is stressed.
2. **64-packet reorder horizon.** The receiver rejects any sequence more
   than 64 ahead of `next_expected` (`reliability.rs::on_receive`,
   `offset > 64 → false`). Under a single loss, effective in-flight is
   capped at 64 packets regardless of cwnd / tx-window; everything beyond
   is dropped on arrival and must be resent.

## Phase 1 — Batched ACK emission (B-1..B-4)

Goal: O(peers) control packets per drain cycle instead of O(2·streams).
No wire-format change; old and new peers interoperate both directions.

### B-1 — Group drained grants by peer
In `spawn_stream_grant_drainer_loop`, group the drained
`(session_id, stream_id) → PendingStreamGrant` entries by session (all
entries for a session share one `peer_addr`). Resolve `ack_seq` per
stream as today.

### B-2 — Pack grants as multi-event frames
Per peer, encode each `StreamWindow` as one event and build **one**
`SUBPROTOCOL_STREAM_WINDOW` packet carrying up to N grant events
(one `next_control_tx_seq()` per packet, not per grant). N is bounded by
the event-frame `event_count` limit and the MTU budget — at 24 B/event
roughly 40–50 grants/packet; overflow spills into additional packets.
Partition-filter check stays per peer (cheaper than today's per stream).

### B-3 — Pack NACKs the same way
Collect `build_nack` results across the peer's drained streams and emit
one `SUBPROTOCOL_STREAM_NACK` packet with one 24-byte `StreamNack` event
per gapped stream. Apply the same packing to the 25 ms proactive path
(`collect_gap_nacks` emission in `spawn_retransmit_loop`, ~5851) and to
the `StreamReset` burst (~5814), which have the same one-packet-per-stream
shape.

### B-4 — Tests + observability
- Regression test pinning the decode loop: a single wire packet carrying
  M `StreamWindow` events applies all M grants (the hazard is described
  at `mesh.rs` ~4769 — verify a test actually covers it; add if not).
  Same for M `StreamNack` events.
- Drainer test: M streams to one peer, one drain cycle ⇒ 1 grant packet
  (+1 NACK packet iff gaps), not 2·M.
- Overflow test: > N streams ⇒ ceil(M/N) packets, no grant dropped.
- Counter for grants-per-packet (or a debug-level log) so the batching
  win is observable.

Drive-by (same files, zero risk): `StreamWindowCodecError` messages still
say "need 16"; the sizes have been 24 since `ack_seq` was added.

## Phase 2 — Range-bound ACKs (R-1..R-6)

Goal: the sender prunes/suppresses retransmits for everything the
receiver holds (kills the RTO flood), and the receiver's reorder horizon
grows from 64 to the real in-flight bound.

### R-1 — Wire form
New `SUBPROTOCOL_STREAM_ACK = 0x0B03` (next free after
`SUBPROTOCOL_STREAM_RESET = 0x0B02`) carrying `StreamAckRanges`:

```
u64 stream_id LE
u64 ack_seq LE              // cumulative next_expected (same as grant's)
[ (u64 start LE, u64 end LE) ]  // received ranges ABOVE ack_seq,
                                // half-open [start, end), ascending,
                                // non-overlapping, count ≤ MAX_ACK_RANGES
```

Variable-size codec in `subprotocol/stream_window.rs` idiom: strict
length validation (`len == 16 + 16·n`, `1 ≤ n ≤ MAX_ACK_RANGES`), reject
truncated/oversize/overlapping/descending input. `MAX_ACK_RANGES = 16`
(272 B max — comfortably one event). Full u64 pairs, not varints —
consistent with every other codec in the file and not worth the
complexity at this rate (≤1 per stream per drain tick).

### R-2 — Receiver state: range set replaces the 64-bit bitmap
In `ReliableStream` (rx side): replace `sack_bitmap: u64` with a bounded,
merged, ordered range set (`VecDeque<(u64, u64)>`, capped at
`MAX_SACK_RANGES = 32`; adjacent/overlapping merge on insert).
- `on_receive` fast path unchanged in shape: `seq == next_expected` with
  an empty range set stays the cheap branch (this runs per data packet —
  bench it, see R-6).
- Acceptance horizon: accept `seq` up to `next_expected + horizon` where
  `horizon` derives from `max_pending` (the H-1-sized retransmit window)
  instead of the fixed 64. A seq that would create a 33rd range is
  rejected (the old 64-limit behavior, just much further out).
- `build_nack` keeps its exact current output (head gap + first-64
  bitmap): fast retransmit only needs the head; deeper gaps surface as
  the head advances. **No NACK wire change.**
- `missing_bitmap`/`has_gaps` derive from the range set.

### R-3 — Sender state: `on_ack_ranges`
New trait method `ReliabilityMode::on_ack_ranges(ack_seq: u64, ranges:
&[(u64, u64)])`, default no-op (FireAndForget untouched).
`ReliableStream` impl:
- First apply the cumulative `on_ack(ack_seq)` prune (reuse the existing
  pop-front + straggler sweep).
- Then remove (or mark `sacked` and skip in `get_timed_out`) every
  `pending` entry whose seq falls inside a range. Removal is simpler and
  correct: a sacked packet can never need retransmit — the receiver
  dedups by `seq < next_expected` OR its range set.
- cwnd: each newly-sacked packet counts as one `grow_cwnd()` (same as a
  cumulative ack today).
- RTT: sample from the highest newly-sacked packet with `retries == 0`
  (Karn preserved).
- Fast-recovery interaction: sacked packets do NOT clear `recover` — only
  a cumulative ack past the recover point does (unchanged, T-1).

### R-4 — Emission (receiver → sender)
In the drainer, next to the grant/NACK for each drained stream: if the
stream is reliable and `has_gaps()`, emit a `StreamAckRanges` event
(batched per peer per Phase 1 — one `SUBPROTOCOL_STREAM_ACK` packet per
peer per cycle). Also from the 25 ms tick for quiet gapped streams (same
walk as `collect_gap_nacks`). No gaps ⇒ nothing emitted; the grant's
`ack_seq` already covers the contiguous case, so loss-free streams see
zero new traffic.

### R-5 — Dispatch + compatibility
- `process_local_packet`: handle `SUBPROTOCOL_STREAM_ACK` alongside the
  window/NACK arms (~4778/4824): decode, quarantine-check
  (`is_grant_quarantined`), `try_stream`, `on_ack_ranges`.
- **Gate emission on peer capability** via the existing capability
  announcement (`SUBPROTOCOL_CAPABILITY_ANN`): advertise e.g.
  `stream.ack-ranges`; senders emit `StreamAckRanges` only to peers that
  advertise it. Receivers accept unconditionally.
- Verify the fall-through for unknown subprotocol ids on
  `CONTROL_STREAM_ID` in `process_local_packet` (the
  `wrong_subprotocol_id_in_payload_dropped` test at `mesh.rs` ~15246
  suggests they drop safely) — the capability gate should make this
  moot, but pin it with a test so an ungated emission can never corrupt
  an old peer's event queue.

### R-6 — Tests + benches
- Unit: range-set insert/merge/cap; horizon acceptance + rejection;
  `on_ack_ranges` pruning, Karn (no RTT sample from retried packets),
  cwnd growth, recover-point non-clearing; codec round-trip + malformed
  rejection (overlap, descending, oversize).
- Integration (reuse the D-5 deterministic drop hook from
  STREAM_RETRANSMIT): drop 1 packet in a 1 000-packet reliable burst with
  RTT > RTO ⇒ assert retransmit count is O(1), not O(window), and cwnd
  does not floor; a >64-packets-ahead reorder test proving the horizon
  lift (pre-fix those arrivals are rejected and resent).
- Criterion bench on `on_receive` in-order hot path (per-data-packet
  cost; per bench-noise-floor guidance judge deltas beyond ±20-30%
  jitter).

## Order
B-1 → B-2 → B-3 → B-4 (ship) → R-1 → R-2 → R-3 → R-5 (dispatch) →
R-4 (emission, capability-gated) → R-6. Phase 2 stages are ordered so the
receiver-accept side lands before any peer emits.

## Out of scope / non-goals
- No change to `StreamWindow` / `StreamNack` / `NackPayload` wire forms
  (both are fixed-size codecs that reject oversize input — extending them
  would break old peers; the new message type + capability gate is the
  compatible path).
- No cross-peer batching (different destinations by definition).
- No delayed-ACK / ACK-frequency tuning beyond the existing 1 ms drainer
  coalescing.
- No change to delivery-order semantics (H-8: arrival-order push, `seq`
  tagged; consumers reassemble).
- FireAndForget streams: untouched (no acks by design; nRPC unary /
  lossy streaming unaffected).
