# Net v0.30 — "Final Countdown"

*Named after Europe's 1986 synth-fanfare arena anthem off* The Final Countdown *— the four-note keyboard hook every stadium air-plays and nobody can name the second verse of, "we're leaving together, but still it's farewell to the ground."*

## An ACK path that finally tells the sender what it holds

Net's reliable transport has always recovered loss correctly, but bluntly. The receiver's only positive signal was a **cumulative** ack (`next_expected`) piggybacked on the `StreamWindow` credit grant, backed by a small **negative** gap bitmap (`StreamNack`) with a hard 64-sequence horizon and an RTO backstop. That is enough on a clean, low-volume path. On a bulk stream under loss it is a cliff: one lost head packet caps usable in-flight at 64 packets until the gap heals, and when the head-gap RTO finally fires, `get_timed_out` retransmits **every** timed-out packet behind it and collapses cwnd to `MIN_CWND` — thousands of spurious resends at the exact moment the link is stressed, because the sender never learned that everything *after* the hole was already received.

v0.30 fixes both ends of that. It is the [stream-ack batching + SACK-range plan](../plans/STREAM_ACK_BATCHING_AND_RANGES_PLAN.md) landed in two phases: **batched control-frame emission** (Phase 1) and **positive SACK-range ACKs** (Phase 2). One branch, the reliability receive index rewritten from a bitmap to a budgeted range set, a new capability-gated wire message, and a control-plane metrics surface.

The organizing observation is the same one that has shaped every release since the substrate stopped being a prototype: **the hard parts already existed — the work was a control loop over them, not new infrastructure.** The event-frame codec already looped multi-event frames on decode; the grant drainer already coalesced per stream (latest-wins); the capability-announcement path already auto-merges transport tags; the SRTT/RTO estimator (RFC 6298) already existed. v0.30 is the encode-side batching those readers were always ready for, plus a positive-ack message and a range index feeding the estimator that already knew how to consume it. No new fold, no new routing concept, no RTT plumbing into the proximity graph.

Below: the wins, grouped by phase, then the review that hardened them.

---

## Phase 1 — batched control-frame emission

Coalescing existed per stream (the latest-wins pending-grant map), but **emission** was one AEAD encrypt + one `sendto` per `(peer, stream)` per drain cycle — ×2 whenever a NACK rode along. With S busy streams to a peer that is up to 2·S control packets per millisecond, all of it re-encrypting and re-syscalling per stream.

v0.30 groups the drained grants **by session** — the session, not the peer address, owns the outbound AEAD cipher, packet pool, and control-seq counter, so it is the only correct unit a batched packet can be built against (two sessions behind one address, e.g. a mid-drain rotation, must not share a packet). Per session it packs each control type into **multi-event frames**: one `StreamWindow` packet carrying up to a payload-budget's worth of grant events, one `StreamNack` packet for the gapped streams, one `StreamReset` packet for the given-up streams — spilling into additional packets only when the payload budget is exceeded. O(2·streams) control packets per cycle collapses to O(sessions). The decode side needed no change — it has always iterated the full event vector, so **old and new peers interoperate both directions with no wire-format change** to grant/NACK/reset.

New observability rides along: `MeshNode::control_plane_stats()` returns a `ControlPlaneStats` with per-type `*_packets_sent` / `*_events_sent` counters, so the achieved coalescing ratio (`events / packets`, 1.0 = no coalescing) is measurable rather than assumed.

---

## Phase 2 — positive SACK-range ACKs

The sender needs to hear what the receiver *holds*, not just where the hole is. Phase 2 adds that, adapted from TCP-SACK / QUIC-ACK-range behavior to Net's per-stream sequences.

**R-1 — wire form.** A new `SUBPROTOCOL_STREAM_ACK = 0x0B03` carries `StreamAckRanges`: a `stream_id`, a cumulative `ack_seq` (every seq `< ack_seq` received), and 1..=`MAX_ACK_RANGES` (16) half-open `[start, end)` runs received strictly *above* `ack_seq`. Ranges are ordered **newest-first** (descending by `end`), fully merged (non-overlapping, non-adjacent) — so truncation to the cap drops the *oldest* ranges, the ones the next cumulative advance covers first anyway. 272 wire bytes max, one event. The decoder validates strictly and rejects malformed input outright (bad length shape, range count, empty/inverted range, a range not strictly above `ack_seq`, any non-descending/unmerged ordering).

**R-2 — the receiver range index.** The 64-bit reorder bitmap becomes a bounded, merged, ordered `VecDeque<(u64, u64)>` capped at `MAX_REORDER_RANGES = 32`. This is an **index, not a payload buffer** — per H-8 the transport pushes out-of-order payloads to consumers in arrival order and they reassemble by seq, so memory is O(ranges), not O(window). The fixed 64-seq acceptance horizon becomes **budgeted**: accept out-of-order seqs up to `next_expected + horizon`, where `horizon` derives from the stream's window budget, floored at the legacy 64. The correctness-critical path is head-collapse: filling the head gap advances `next_expected` *through* the range that just became contiguous (`ack_seq=10`, ranges `[(11,21)]`, receive 10 ⇒ `ack_seq=21`, ranges empty). The legacy NACK bitmap is now *derived* from this index and stays byte-identical for every state the old 64-seq horizon could express, so **the NACK wire form is unchanged**.

**R-3 — the sender prunes.** A new `ReliabilityMode::on_ack_ranges(ack_seq, ranges)` applies the cumulative prune, then **removes** every `pending` entry whose seq falls inside a range — not mark-and-skip; a positive SACK means the receiver has it, so it leaves in-flight accounting and is never retransmit-eligible again. This is the whole point: after one head loss the window holds **only the genuinely-missing packets**, so `get_timed_out` resends O(lost) instead of O(window), and cwnd does not floor. RTT sampling stays Karn-correct (only never-retransmitted packets sample); cwnd growth from a delayed SACK covering thousands of packets is **capped per invocation** so it can't step-function into a burst (there is no pacer); `recover` is not cleared by ranges (the head gap is by definition still missing).

**R-4/R-5 — emission + capability gating.** The receiver emits `StreamAckRanges` (batched per session per Phase 1) for gapped streams, and the sender emits **only** to peers advertising the `net.reliable.stream_ack_ranges@1` capability tag; peers that don't advertise it get the legacy cumulative + NACK path. **Receivers accept unconditionally** — the capability is optimization negotiation, not authority. `MeshNodeConfig::enable_stream_ack_ranges` (default `true`, `with_stream_ack_ranges(false)` kill switch) turns the feature off wire-wide for a node's sends and advertisements.

**The result, pinned.** `on_ack_ranges_suppresses_rto_flood_after_head_loss` (unit) and `tests/stream_ack_ranges.rs` (e2e, capability-gated engage-under-loss + old-peer interop + kill switch) prove one head loss under a 1000-packet in-flight RTO-resends **1 packet, not 1000**, with no cwnd floor. `benches/reliability.rs`: the in-order `on_receive` hot path is ~1.6 ns after the range-index rewrite; the SACK prune of a 1000-packet window is ~40 µs (loss-path only).

---

## The hardening pass — what the ACK review forced

A [dedicated code review](../misc/CODE_REVIEW_2026_07_02_ACKS_PLAN_CANDIDATES.md) of the landed ack-batching branch ran ten finder angles into ~50 candidates, deduped to ~17 clusters, and adversarially verified each. The pure cores held — the range arithmetic (half-open intervals, head-collapse, merge/bridge, bitmap derivation, codec validation) checked out against its invariants. The real findings clustered where the *emission* path met concurrency and the wire: a cache that outlived the fact it cached, three lock acquisitions where one snapshot was meant, and a metric that claimed more than the network can guarantee. Every fix landed with a regression test; the full crate lib suite (4,559 tests) and clippy are green.

**A gate verdict cached before its own evidence arrived (the headline fix).** The per-peer ack-ranges capability gate reads through a 5 s TTL cache. But the retransmit tick consults it for every peer from its first 25 ms tick — *before* the peer's capability announcement has folded — so the opening loss episode of a fresh transfer would cache `false` and pin the legacy RTO-flood path for a full 5 s, exactly when SACK ranges matter most. A peer downgrade could likewise strand a stale `true`. The cache is now **invalidated event-driven**: `handle_capability_announcement` drops the peer's entry the instant its announcement folds, so both the connect-time race and the downgrade self-heal at once instead of on the TTL. Pinned by `ack_ranges_gate_reresolves_after_announcement_invalidation`.

**Three locks where one snapshot was meant.** `build_session_control_events` read a stream's `ack_seq`, NACK, and SACK ranges under three separate reliability-lock acquisitions, so a head-fill landing mid-build could ship a `StreamWindow{ack_seq=10}` next to a `StreamAckRanges{ack_seq=21}` in the same cycle — mutually contradictory frames. It now reads all three from **one `try_stream` + one lock** per stream, which is both strictly consistent and ~3× cheaper on the 1 kHz drainer path. The 25 ms proactive tick had the same shape doubled — it walked every stream twice (`collect_gap_nacks` then `collect_ack_ranges`), locking each twice and snapshotting the range index at two instants — now folded into one `NetSession::collect_gap_reports` walk.

**A "hostile peer" counter that honest peers trip.** `protocol_anomalies` counts a NACK naming a seq a prior SACK acknowledged, and was documented as proof of a buggy/hostile peer. The single-snapshot build above removes the *same-cycle* contradiction, but the NACK and SACK for one gap still ride **separate datagrams that reorder independently on the wire** — a stale NACK arriving after a fresher SACK trips the counter on any lossy/jittery link. The behavior is harmless (the packet is already gone from `pending`); the docs were the bug. Reframed as a best-effort signal — "alert on sustained growth, not a single count."

**Variable-size events shipping empty packets.** SACK packets were chunked by a worst-case fixed count (`MAX_ACK_RANGES` × 16 B ⇒ ~29 events/packet), so a batch of typical one-range events shipped datagrams ~85 % empty. A shared `emit_control_chunks` helper — one path for the six previously copy-pasted (and already drifted) chunk-emit sites — now packs by **actual framed size** via `pack_control_events`, filling each datagram. Plus the small wins: `on_ack_ranges` reuses its `last_sacked` buffer instead of allocating per SACK, and the three fixed-size decoders share one `require_exact_len` gate (the exact drift surface that once let a stale "need 16" message outlive a 24-byte grant).

**Refuted, honestly.** Not every candidate survived. A "stale session address after migration" cluster was **refuted** — `session_id → peer_addr` is an immutable binding (every rebind builds a fresh session id), so grouping on session id and keeping the first address is behaviorally identical to the old per-grant addressing. A "reset-batch loss amplification" finding was refuted — per-reset loss was already permanent pre-batching; batching changes only fate-correlation, and `StreamReset` is itself a fast-fail optimization over the peer-side timeout. An "`Instant::now()` test panic" was refuted for this repo — Linux `Instant` doesn't underflow below the boot epoch and CI is Linux-only. All recorded with rationale in the [review resolution](../misc/CODE_REVIEW_2026_07_02_ACKS_PLAN_CANDIDATES.md#resolution-2026-07-02-post-verification).

---

## What's deferred (honestly)

- **Grant-chunk loss correlation.** Batching means one lost grant *datagram* now carries many streams' credit grants instead of one — a real correlation change. But grants are authoritative and re-minted on the next accepted packet, the committed-flush stall budget backstops a truly wedged stream, and on-wire datagram loss was never distinguishable from a `sendto` that returned `Ok`. The syscall/packet reduction is the intended trade; flagged for revisit if credit-blocked-idle stalls are observed in the field.
- **The reorder-horizon burst under deep pure reordering.** The lifted horizon is the feature — but on a >64-deep reorder (no loss) a large-window stream can now emit a wider legacy NACK than the old 64-cap allowed. The review judged this "differently bad, arguably better" than the old drop-and-RTO-collapse (bounded ≤65-packet burst + one cwnd halving vs. a full-window flood + `MIN_CWND` stall), and it cannot escalate to a spurious reset. No code change; noted.
- **Auto-announce on start.** The capability tag only propagates once the node broadcasts an announcement — an explicit `announce_capabilities`, or the `start_arc` reannounce loop within `capability_reannounce_interval`. A bare `start()` that never announces keeps peers on the legacy path. Documented on `ACK_RANGES_CAPABILITY_TAG` / `enable_stream_ack_ranges`; making the node auto-announce at startup is a global-behavior change left as a deliberate decision, not slipped into a cleanup pass.
- **Independent tx/rx windows.** `reorder_horizon` reuses the tx-window-derived `max_pending` as the rx acceptance budget — correct only because a stream is constructed with one window feeding both. A caveat marks exactly what to change (an explicit rx-budget argument) if they ever diverge.
- **`ack_delay` / delayed-ACK tuning.** Out of scope; the 1 ms drainer coalescing is the only ACK cadence. `ack_delay_us` can ride the message additively later if RTT tuning demands it.

---

## Breaking changes

v0.30 is **additive**. A new subprotocol message type behind a capability gate, a new defaulted config field, and new *defaulted* trait methods — existing callers are untouched, and a peer on an older substrate that never advertises the tag interoperates on the unchanged legacy path.

**`ReliabilityMode` gained two methods, both defaulted.** `on_ack_ranges` (default no-op) and `build_ack_ranges` (default empty) — `FireAndForget` and any external impl compile and behave exactly as before; the range machinery is opt-in on `ReliableStream`.

**`MeshNodeConfig` gained `enable_stream_ack_ranges` (default `true`)** plus the `with_stream_ack_ranges` builder. Existing configs built via `Default` / `..` get the feature on; a peer that never advertises still gets legacy behavior toward it, so "on by default" is safe.

**No wire risk to existing frames.** `StreamWindow` / `StreamNack` / `StreamReset` / `NackPayload` wire forms are **unchanged** — batching packs more events into the same fixed-size frames the decode side already iterated. The only new bytes on the wire are the capability-gated `SUBPROTOCOL_STREAM_ACK = 0x0B03`, which unknown/old peers drop safely as an unknown subprotocol id.

**New public surface:** `ControlPlaneStats` + `MeshNode::control_plane_stats()`, `ACK_RANGES_CAPABILITY_TAG`, `StreamAckRanges` (+ `SUBPROTOCOL_STREAM_ACK`, `MAX_ACK_RANGES`), the per-stream `ReliableStream` observability accessors (`out_of_order_accepted` / `_dropped_horizon` / `_dropped_capacity`, `reorder_ranges`, `protocol_anomalies`), `ReliabilityMode::{on_ack_ranges, build_ack_ranges}`, and `NetSession::{collect_gap_reports, GapReport}`.

---

## How to upgrade

1. **Pull the release** — no code change required. Streams keep working; without the capability advertised on both ends, recovery uses the unchanged cumulative-ACK + NACK + RTO path exactly as before.
2. **To engage SACK ranges**, make sure both peers advertise capabilities: call `MeshNode::announce_capabilities` after connecting (as the e2e tests do), or run via `start_arc` so the reannounce loop broadcasts within `capability_reannounce_interval`. The `net.reliable.stream_ack_ranges@1` tag auto-merges into the announcement while `enable_stream_ack_ranges` is `true`.
3. **To turn it off wire-wide** for a node (sends *and* advertisements), set `MeshNodeConfig::with_stream_ack_ranges(false)`. Receiving stays unconditional; this only stops the node emitting and advertising.
4. **To measure the win**, read `MeshNode::control_plane_stats()` — the `*_events_sent / *_packets_sent` ratio is the achieved batching factor, and `ack_range_packets_sent` confirms the SACK path is engaging under loss.
5. **Everyone else** gets the new surfaces with no behavior change to existing paths.

---

## Dependency updates

No first-party dependency was added or removed; the crate version bumps `0.29.1 → 0.30.0` (propagated across the CLI, deck, and SDK manifests). The Cargo.lock changes since v0.29.0 are transitive / tooling refreshes pulled in by automated dependency updates:

- `rand` 0.10.1 → 0.10.2 (transitively shifts `getrandom` 0.4.3 → 0.3.4 in the resolved graph)
- `time` 0.3.52 → 0.3.53
- `xxhash-rust` 0.8.15 → 0.8.16
- `indicatif` 0.18.5 → 0.18.6, `console` 0.16.3 → 0.16.4, `clap_complete` 4.6.5 → 4.6.7 (CLI UX)
- `napi` 3.9.4 → 3.10.0, `napi-derive` 3.5.7 → 3.5.8, `napi-derive-backend` 5.0.5 → 5.1.0 (Node binding tooling)
- `libredox` 0.1.17 → 0.1.18

None touch the transport, fold, or reliability paths; all are lockfile-only refreshes.

---

Released 2026-07-02.

## License

See [LICENSE](../../LICENSE).
