# Stream ACKs — batched emission + range-bound (SACK-range) ACKs

**Status:** DONE (2026-07-02). Phase 1 (batched control-frame emission)
landed in `97662a9fe`; Phase 2 (positive SACK-range ACKs) in the
follow-up commit. Verified: 4552 lib tests (9 new R-2/R-3 unit tests, 8
Phase-1 batching tests, 7 codec tests), `tests/stream_ack_ranges.rs`
(capability-gated engage-under-loss + old-peer interop + kill switch),
all transfer/nrpc/three-node suites incl. the 4 MiB concurrent sweep,
clippy clean. `benches/reliability.rs`: in-order `on_receive` hot path
~1.6 ns post-rewrite; SACK prune of a 1000-packet window ~40 µs
(loss-path only).

**Outcome:** grant/NACK/reset control frames batch per session
(O(sessions) packets per drain cycle, was O(2·streams));
`SUBPROTOCOL_STREAM_ACK = 0x0B03` carries half-open newest-first SACK
ranges, emission gated on the auto-announced
`net.reliable.stream_ack_ranges@1` capability tag
(`MeshNodeConfig::enable_stream_ack_ranges` kill switch); the receiver's
64-seq reorder bitmap became a budgeted range index
(horizon = window-derived, floor 64, `MAX_REORDER_RANGES = 32`); SACKed
packets are removed from the retransmit window, so one head loss under
large in-flight RTO-resends O(lost) instead of O(window) — pinned by
`on_ack_ranges_suppresses_rto_flood_after_head_loss` and the e2e test.
Observability via `MeshNode::control_plane_stats()` + per-stream
out-of-order / anomaly counters.

## Background — how ACKs work today (verified 2026-07-02)

There is no standalone ACK packet. Current reliability is **cumulative
ACK + small negative gap bitmap + RTO fallback**:

- The receiver's cumulative ack (`next_expected`) piggybacks as the
  `ack_seq` field of the `StreamWindow` credit grant
  (`subprotocol/stream_window.rs:70-83`; 24-byte payload: `stream_id`,
  `total_consumed`, `ack_seq`; `SUBPROTOCOL_STREAM_WINDOW = 0x0B00`,
  rides `CONTROL_STREAM_ID`). `total_consumed` is flow-control credit;
  `ack_seq` is the transport ack. **These stay semantically separate
  even though they share a wire frame and drainer.**
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

This is enough for correctness on small/clean paths, but not for
high-throughput reliable transport under loss/reordering.

## The gap

**A. No batched emission.** Coalescing exists per-stream (latest-wins
map), but emission is one AEAD encrypt + one `sendto` per (peer, stream)
per drain cycle, ×2 when a NACK rides along. With S active streams to a
peer that is up to 2·S control packets per millisecond. The decode side
**already loops multi-event frames** for both `StreamWindow` and
`StreamNack` (`mesh.rs` ~4780 / ~4827), and
`RxCreditState::on_bytes_consumed`'s doc explicitly anticipates: *"a
future enhancement can batch grants without changing the wire format"*
(`session.rs` ~1125). Only the encode side is missing. Batching reduces
AEAD encrypts, `sendto` syscalls, control-packet count, scheduler wake
pressure, and per-stream control amplification.

**B. Out-of-order data cannot be positively acknowledged.** If the
receiver holds 1..100 and 102..10000 but is missing 101, the sender only
learns `ack_seq = 101` (+ a NACK naming 101). It does not know
102..10000 are safely received, so its retransmit window (`pending`, up
to `MAX_RETRANSMIT_WINDOW = 16_384` entries) retains a huge amount of
already-received data. When the head-gap packet's RTO fires,
`get_timed_out` (`reliability.rs:683-721`) retransmits **every**
timed-out packet and collapses cwnd to `MIN_CWND`. One lost packet on a
bulk stream ⇒ potentially thousands of spurious retransmits at the exact
moment the link is stressed.

**C. 64-packet reorder horizon is an artificial throughput cliff.** The
receiver rejects any sequence more than 64 ahead of `next_expected`
(`reliability.rs::on_receive`, `offset > 64 → false`). One lost head
packet caps usable in-flight at 64 packets until the gap heals — fighting
the whole point of large windows / cwnd / high-throughput streams.

## Phase 1 — Batched control-frame emission

Goal: O(peers) control packets per drain cycle instead of O(2·streams).
No wire-format change; old and new peers interoperate both directions.

### Phase 1A — encode batching + metrics (B-1..B-4)

**B-1 — Group drained grants by session.** In
`spawn_stream_grant_drainer_loop`, group the drained
`(session_id, stream_id) → PendingStreamGrant` entries **by
`session_id`** — the grouping key must uniquely map to the outbound
encrypted session context (builder key, control-seq counter, peer addr).
Do NOT group by peer addr alone: the session is what owns the AEAD state.

**B-2 — Pack grants as multi-event frames.** Per session, encode each
`StreamWindow` as one event and build **one**
`SUBPROTOCOL_STREAM_WINDOW` packet carrying up to N grant events (one
`next_control_tx_seq()` per packet, not per grant). N is bounded by the
event-frame `event_count` limit and the MTU budget — at 24 B/event
roughly 40–50 grants/packet; overflow spills into additional packets.
Partition-filter check per session.

**B-3 — Pack NACKs the same way.** Collect `build_nack` results across
the session's drained streams and emit one `SUBPROTOCOL_STREAM_NACK`
packet with one 24-byte `StreamNack` event per gapped stream. Apply the
same packing to the 25 ms proactive path (`collect_gap_nacks` emission in
`spawn_retransmit_loop`, ~5851) and to the `StreamReset` burst (~5814),
which have the same one-packet-per-stream shape.

**B-4 — Metrics.** Add counters now so the improvement (and Phase 2's)
is provable, not vibes:
- `stream_grant_packets_sent`, `stream_grant_events_sent`
- `stream_nack_packets_sent`, `stream_nack_events_sent`
- `retransmit_packets_sent`
- (Phase 2 adds: `stream_ack_range_packets_sent` / `_events_sent`,
  `ack_ranges_per_packet`, `spurious_retransmit_suppressed_by_sack`,
  `out_of_order_packets_accepted`, `out_of_order_packets_dropped_horizon`,
  `reorder_range_count_max`)

Drive-by (same files, zero risk): `StreamWindowCodecError` messages still
say "need 16"; the sizes have been 24 since `ack_seq` was added.

### Phase 1B — regression / stress tests (B-5)

| Test | Purpose |
|---|---|
| multi-grant decode applies all M grants | pins existing reader behavior |
| M stream grants to same peer ⇒ 1 packet | verifies batching |
| overflow spills by event-count/MTU budget, no grant dropped | avoids overlarge-packet bug |
| M gapped streams ⇒ 1 NACK packet | verifies second control path |
| mixed sessions to same peer addr stay separate packets | catches bad grouping-key assumptions |
| grants for different peers remain separate | avoids wrong routing |
| 100-stream stress: drainer emits ≤ ceil(100/N) packets | end-to-end cadence bound |

**Commit Phase 1** once green (lib tests + transfer/fairness/nrpc suites).

## Phase 2 — Range-bound ACKs (positive SACK)

Goal: the sender prunes retransmit state for everything the receiver
holds (kills the RTO flood), and the receiver's reorder horizon grows
from 64 to a real, budgeted bound. TCP-SACK/QUIC-ACK-range behavior
adapted to Net's per-stream sequences.

### R-1 — Wire form + semantics

New `SUBPROTOCOL_STREAM_ACK = 0x0B03` (next free after
`SUBPROTOCOL_STREAM_RESET = 0x0B02`) carrying `StreamAckRanges`:

```
u64 stream_id LE
u64 ack_seq LE                   // cumulative: all seq < ack_seq received
[ (u64 start LE, u64 end LE) ]   // half-open [start, end); received runs
                                 // strictly above ack_seq; DESCENDING by
                                 // end (newest first); non-overlapping,
                                 // non-adjacent; 1 ≤ count ≤ MAX_ACK_RANGES
```

**Semantics, stated precisely:** `ack_seq` cumulatively acknowledges all
sequence numbers `< ack_seq`. `ranges` selectively acknowledge received
sequence numbers `> ack_seq` (`ack_seq` itself is by definition the
missing head — it is never inside a range, and the cumulative run is
never duplicated as a range). Example: `ack_seq = 101`,
`ranges = [(102, 10001)]` ⇒ everything <101 received, 101 missing,
102..=10000 received.

Half-open `[start, end)` — matches Rust convention, avoids off-by-one.
Newest-first ordering means truncation to `MAX_ACK_RANGES` drops the
*oldest* ranges (the ones a cumulative advance will cover first anyway).

Codec in `subprotocol/stream_window.rs` idiom: strict validation —
`len == 16 + 16·n`, `1 ≤ n ≤ MAX_ACK_RANGES = 16`, every `start < end`,
every `start > ack_seq`, strictly descending, no overlap/adjacency.
Reject malformed input outright. Full u64 pairs, not varints (272 B max
— one event; consistent with every codec in the file). No `ack_delay`
field for now — grants don't carry one either; add `ack_delay_us: u32`
later if RTT tuning demands it.

### R-2 — Receiver: received-range index replaces the 64-bit bitmap

In `ReliableStream` (rx side): replace `sack_bitmap: u64` with a bounded,
merged, ordered range set — the **receive reorder index** (Net's
transport receiver does not buffer out-of-order payloads; per H-8 they
are pushed in arrival order and consumers reassemble by seq — so this is
an index, O(MAX_REORDER_RANGES) memory, not a payload buffer).
`VecDeque<(u64, u64)>` capped at `MAX_REORDER_RANGES = 32`; no tree
needed at this cap.

Required operations:
- `insert_received(seq)` with adjacent/overlap merge;
- **head-collapse on gap fill** — when `seq == next_expected` arrives,
  `next_expected` must advance *through* any range now starting at it
  (e.g. `ack_seq=10`, `ranges=[(11,21)]`, receive 10 ⇒ `ack_seq=21`,
  ranges empty). This collapse path is correctness-critical;
- `build_ack_ranges(max_ranges)` — newest-first, for R-4 emission;
- `has_gaps()`; `missing_bitmap` for the unchanged legacy NACK derives
  from the first 64 seqs of the range set.

Fast path preserved: `seq == next_expected && ranges empty` stays the
cheap branch (runs per data packet — benched in R-6).

**Acceptance horizon: budgeted, not fixed.** Accept out-of-order seqs up
to `next_expected + horizon` where `horizon` derives from the stream's
**receive-side window budget** (rx credit `window_bytes /
MIN_TRACKED_PACKET_BYTES`, clamped to `[64, MAX_REORDER_PACKETS =
16_384]`) — NOT from the sender's `max_pending` (the receiver must not
blindly accept whatever the sender is willing to track). Additionally, an
insert that would create a 33rd range is rejected (counted in
`out_of_order_packets_dropped_horizon`). The invariant: out-of-order
acceptance is bounded by the receiver's own configured budget.

Legacy `build_nack` output is byte-identical for all states expressible
today; **no NACK wire change.** NACK's post-SACK role shrinks to
fast-retransmit hint for the head gap + quiet-stream recovery.

### R-3 — Sender: `on_ack_ranges` removes SACKed packets

New trait method `ReliabilityMode::on_ack_ranges(ack_seq: u64, ranges:
&[(u64, u64)])`, default no-op (FireAndForget untouched).
`ReliableStream` impl:

- First apply the cumulative `on_ack(ack_seq)` prune (existing pop-front
  + straggler sweep).
- Then **remove** every `pending` entry whose seq falls inside a range —
  not mark-and-skip. A positive SACK means the receiver has it; it is
  acknowledged (call it `acked_by_sack` internally if useful), leaves
  in-flight accounting, and must never be retransmit-eligible again.
  Stream-level ordering metadata (if any ever needs it) is separate from
  packet-level retransmit state.
- **cwnd, conservatively:** newly-SACKed packets count as acked for cwnd
  growth via the normal per-ack accounting — but cap the growth applied
  per `on_ack_ranges` invocation (e.g. at most one RTT-worth /
  `max_pending`-bounded step) so a delayed SACK covering thousands of
  packets can't trigger a burst; there is no pacer to absorb it.
- **RTT (Karn):** sample only packets never retransmitted
  (`retries == 0`); use the newest newly-acknowledged such packet with a
  known send timestamp. No invented ack-delay compensation. **Scope:**
  this is the existing transport-local SRTT/RTO estimator
  (`ReliableStream::update_rto`, RFC 6298) extended from cumulative ACKs
  to range ACKs — nothing more. It drives retransmission/loss recovery
  only and coexists with the proximity graph, which keeps owning
  routing/path priors.
- **Contradiction rule:** if a NACK claims a seq missing that a prior
  SACK range acknowledged, the positive ACK wins for that seq — the
  packet is gone from `pending`, so `on_nack` naturally finds nothing;
  count a `protocol_anomaly` metric rather than resurrecting state.
  Receiver-side producer logic can't emit both (NACK derives from the
  same range set), so an anomaly indicates a buggy/hostile peer.
- Sacked packets do NOT clear `recover` — only a cumulative ack past the
  recover point does (unchanged, T-1).

### R-4 — Emission (receiver → sender)

Emit a `StreamAckRanges` event (batched per session per Phase 1, one
`SUBPROTOCOL_STREAM_ACK` packet per session per cycle) when:
- the drained stream has gaps (`has_gaps()`);
- a retransmitted / head-gap packet arrives and **collapses ranges** — a
  material change the sender wants promptly. (The grant path already
  fires on every accepted packet, carrying the advanced `ack_seq`; verify
  that covers the collapse case and the sender prunes fast — if so, no
  extra emission needed here, and the plan's default is to rely on it.)
- the 25 ms tick fires with unresolved gaps (same walk as
  `collect_gap_nacks`).

No gaps ⇒ nothing emitted; the grant's `ack_seq` covers the contiguous
case, so loss-free streams see zero new control traffic.

### R-5 — Dispatch + capability gating

- `process_local_packet`: handle `SUBPROTOCOL_STREAM_ACK` alongside the
  window/NACK arms (~4778/4824): decode, quarantine-check
  (`is_grant_quarantined`), `try_stream`, `on_ack_ranges`.
- **Capability:** advertise `net.reliable.stream_ack_ranges@1` via the
  existing capability announcement (`SUBPROTOCOL_CAPABILITY_ANN`).
  Sender emits `StreamAckRanges` only to peers that advertise it; peers
  that don't ⇒ legacy cumulative + NACK only. **Receivers accept
  unconditionally**, including from peers that never announced —
  capability here is optimization negotiation, not authority.
- Unknown subprotocol ids drop safely on old peers
  (`wrong_subprotocol_id_in_payload_dropped`, `mesh.rs` ~15246) — an
  accidental ungated send is survivable but wasteful; gate anyway, and
  pin the drop behavior with a test.

### R-6 — Tests + benches

- Unit: range insert/merge/cap; head-collapse advance (the
  `ack_seq=10, ranges=[(11,21)], recv 10 ⇒ ack_seq=21` case); horizon
  acceptance/rejection + budget derivation; `on_ack_ranges` removal,
  Karn, capped cwnd growth, recover-point non-clearing, SACK-vs-NACK
  contradiction; codec round-trip + malformed rejection (overlap,
  adjacency, ascending order, `start ≤ ack_seq`, oversize, truncated).
- Integration (reuse the D-5 deterministic drop hook from
  STREAM_RETRANSMIT): **the killer demo — one head loss under large
  in-flight (≫64 packets) causes O(1) retransmits, not O(window), and
  cwnd does not floor.** Both a test and a benchmark. Plus a
  >64-packets-ahead reorder test proving the horizon lift, and a mixed
  old/new-peer interop test (no capability ⇒ no `StreamAckRanges` on the
  wire, legacy behavior intact).
- Criterion bench on `on_receive` in-order hot path (per-data-packet
  cost; judge deltas beyond the ±20-30% jitter noise floor).

## Order (explicit landing sequence)

1. **Phase 1A** — batch `StreamWindow` + `StreamNack` (+ resets), fix
   error strings, add metrics.
2. **Phase 1B** — regression/stress tests. → **commit Phase 1.**
3. **Phase 2A** — receiver range set replaces the bitmap internally;
   still emits *old NACK only*; horizon lift under budget; unit tests.
4. **Phase 2B** — sender `on_ack_ranges` + `SUBPROTOCOL_STREAM_ACK`
   dispatch handler: parse + prune, **not emitted yet**; unit tests.
5. **Phase 2C** — capability advertise/gate, batched emission, mixed
   old/new-peer integration + killer-demo test. → **commit Phase 2.**

Internal correctness lands before network behavior changes; the
receive/parse side lands before any peer emits.

## Out of scope / non-goals

- No change to `StreamWindow` / `StreamNack` / `NackPayload` wire forms
  (fixed-size codecs that reject oversize input — extending them would
  break old peers; the new message type + capability gate is the
  compatible path).
- No cross-peer batching (different destinations by definition).
- No `ack_delay` field / delayed-ACK tuning beyond the existing 1 ms
  drainer coalescing (revisit if RTT tuning demands it).
- No pacer — the capped-growth rule in R-3 is the burst guard.
- No new route/proximity RTT concept. ACK/SACK-derived RTT stays
  transport-local (per-stream SRTT/RTO for loss recovery); the proximity
  graph keeps owning routing/path priors. Feeding ACK-derived
  observations back into graph metrics is a possible later enhancement,
  explicitly out of scope here.
- No change to delivery-order semantics (H-8: arrival-order push, `seq`
  tagged; consumers reassemble) — the range set is an index, not a
  payload reorder buffer.
- FireAndForget streams untouched (no acks by design; nRPC unary /
  lossy streaming unaffected).
