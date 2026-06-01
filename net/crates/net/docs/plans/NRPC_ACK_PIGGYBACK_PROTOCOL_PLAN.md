# nRPC Ack-Piggyback — Protocol Plan

> Spun out of [`NRPC_QPS_CONCURRENCY_SCALING_PLAN.md`](NRPC_QPS_CONCURRENCY_SCALING_PLAN.md),
> which proved the unary QPS ceiling is bound by the **single per-node recv loop**
> (syscall/wake-latency bound) and that the one lever which actually hits it —
> dropping the standalone StreamWindow grant on unary — is **unsafe today because
> the grant *is* the ACK**. This plan designs the safe form: carry the reliability
> ack on the data packet so unary needs no separate grant packet. **Design doc —
> not yet approved for implementation** (wire/interop change).

## Problem

A **reliable unary RPC is four packets**, not two:

| # | direction | packet | processed by |
|---|-----------|--------|--------------|
| 1 | caller → server | REQUEST (data, reliable) | server recv loop |
| 2 | server → caller | StreamWindow grant for the **request** stream (acks #1) | caller recv loop |
| 3 | server → caller | RESPONSE (data, reliable) | caller recv loop |
| 4 | caller → server | StreamWindow grant for the **reply** stream (acks #3) | server recv loop |

Each recv loop (the bottleneck named by the QPS plan) processes **2 packets per
call**: a data packet and a grant. The grant exists because StreamWindow is the
**sole** positive-ACK mechanism (`on_ack` has one production caller,
`mesh.rs:4128`; data headers carry no ack field, `protocol.rs:114-147`; retransmit
is timer-driven, `mesh.rs:5087` / `reliability.rs:683`). Dropping it without
replacing the ack ⇒ 50 ms-RTO retransmit ×3 ⇒ StreamReset.

The grant carries two things (`subprotocol/stream_window.rs:70-83`):
`total_consumed` (transport credit) **and** `ack_seq` (cumulative reliable ack).
For unary, the only one that *must* travel is `ack_seq` — credit is irrelevant when
the sender will never send a second packet.

## Goal

Collapse the 4-packet unary round trip to **2** by piggybacking the reverse-stream
`ack_seq` onto the data packet that already flows the other way. Each recv loop then
processes **1 packet per call** instead of 2 — directly halving the load on the
resource the QPS plan identified as the wall. Streaming traffic keeps standalone
grants (it needs the credit cadence); only the redundant ack-only grant is removed.

**Target:** unary `nrpc_qps c16/32B` ceiling materially above the current ~84 K
(structural halving of recv-loop packets; actual gain gated by Phase 3 re-bench).

## Non-goals

- Changing the credit/flow-control model for streaming. Standalone grants stay for
  streams that actually consume window.
- Touching the recv-loop syscall structure (that's the *other* lever — batched
  recv — tracked in the QPS plan, not here).

---

## Design options

### Option A — `ack_seq` field in `NetHeader`
Add `ack_seq` (+ `ack_stream_id`) to the 68-byte header so **every** reliable data
packet cumulatively acks the reverse stream.
- **+** General — benefits all reliable traffic, not just RPC; conceptually clean.
- **−** Header grows (68 → 80+, re-aligns the struct); changes parse/encode and the
  **AEAD AAD** (`header.aad()`); breaks wire compat for *every* packet; forces a
  version bump touching all peers at once.
- **Verdict:** highest blast radius. Hold unless we want acks on all reliable
  traffic generally.

### Option B — piggyback a transport-ack **event** in the data frame ✅ recommended
Append one control event — `DISPATCH_STREAM_ACK` (new) — to the RPC RESPONSE (and
REQUEST, for the reverse leg) event frame, carrying `{stream_id, ack_seq,
total_consumed}` for the *other* direction's stream. The recv loop's event scan
applies it (`on_ack` + `apply_authoritative_grant`) to the named stream, then
delivers the data event to the RPC layer as normal.
- **+** No per-packet header tax — only packets that choose to ack pay ~24 bytes.
- **+** **Strong precedent:** `DISPATCH_RPC_REQUEST_GRANT` (0x16) is already an
  event-carried grant (`cortex/rpc.rs:101`, emitted `mesh_rpc.rs:1505`, consumed
  `cortex/rpc.rs:3820`). The event/EventMeta dispatch-type machinery already exists.
- **+** Cross-stream is fine — the embedded message names its own `stream_id`, so it
  applies to the request stream even while riding a reply-channel packet.
- **−** Interop: a peer that doesn't grok the event must still ack. Requires
  capability negotiation (below) so the server only *suppresses* the standalone
  grant once the caller is known to apply embedded acks.
- **Verdict:** targeted, lowest blast radius, precedented. **Recommended.**

### Option D — make unary REQUEST unreliable; lean on RPC-level retry
Drop transport reliability for single-packet unary; no retransmit window ⇒ no ack ⇒
no grant.
- **+** Smallest transport change; removes all 2 grants for unary outright.
- **−** Moves loss recovery to the RPC call-timeout layer (≫ 50 ms RTO) → much worse
  tail latency on real loss; weakens the reliability guarantee callers expect.
- **Verdict:** tempting but a semantic regression. Keep as fallback only.

---

## Recommended design (Option B) — detail

### Wire
- New event dispatch type `DISPATCH_STREAM_ACK` (next free id alongside
  `cortex/rpc.rs:101`). Payload = the existing 24-byte `StreamWindow` encoding
  (`stream_id`, `total_consumed`, `ack_seq`) so we reuse `StreamWindow::{encode,
  decode}` verbatim — no new codec.
- The event rides inside the normal event frame; `EVENT_COUNT` already covers it;
  AEAD/AAD unchanged (it's payload, not header).

### Sender (server response leg, mirror on caller request leg)
- When emitting the RESPONSE for a call whose peer **supports piggyback**, prepend a
  `DISPATCH_STREAM_ACK` event for the request stream carrying
  `rx_ack_seq()` (`reliability.rs:94`) + `total_consumed`, and **suppress** the
  standalone StreamWindow grant for that stream (skip the
  `pending_stream_grants` enqueue at `mesh.rs:4711` for this stream this tick).
- If the peer does **not** support piggyback (or no response is imminent — e.g.
  streaming), fall through to today's standalone grant. No regression.

### Receiver
- In the event-dispatch scan (`process_local_packet`, after decrypt, around
  `mesh.rs:4727+`), handle `DISPATCH_STREAM_ACK` **before** RPC delivery: decode,
  apply `on_ack` + `apply_authoritative_grant` to the named stream (reusing the
  exact logic at `mesh.rs:4117-4129`, incl. the grant-quarantine guard), then
  continue to the data event. Idempotent + monotonic, so a duplicate (embedded +
  a racing standalone) is harmless.

### Interop / rollout (the load-bearing part)
1. **Negotiate** a `piggyback_ack` capability at handshake (or via the existing
   capability announce). Default off.
2. **Phase 1 — additive:** peers that support it *also* apply embedded acks, but
   senders keep emitting standalone grants. Zero behavior change; validates the
   embedded path in production traffic.
3. **Phase 2 — suppress:** once *both* peers advertise support, the sender drops the
   standalone grant for the piggybacked stream. The capability gate guarantees the
   receiver will apply the embedded ack, so no unacked-retransmit window opens.
4. Old peers never negotiate → always get standalone grants → unaffected.

---

## Correctness invariants (must hold)

- **Every reliable packet still gets acked.** Suppression is gated on the receiver
  provably applying the embedded ack (capability). Never suppress toward a peer that
  hasn't advertised support.
- **Monotonic, authoritative ack apply** is preserved — embedded acks go through the
  same `apply_authoritative_grant` (monotonic) + `on_ack` path; reordering or
  duplication (embedded vs a late standalone) cannot regress the window.
- **Grant-quarantine** (`is_grant_quarantined`, `mesh.rs:4117`) must wrap the
  embedded apply too — a piggybacked ack for a just-closed/reopened stream must be
  dropped identically.
- **Credit for streaming is unchanged** — only the *ack-only* grant on a stream that
  isn't consuming window is removed. Any stream still consuming ≥ window keeps its
  standalone grant cadence.
- **Fragmentation:** the ack event must land on a packet the receiver fully
  reassembles before applying (apply after the data event is decoded, same as
  today's frame iteration).

## Risks

- **Interop rollout bug** = retransmit storm (the exact failure the QPS plan caught).
  Mitigated by capability-gated suppression + the additive Phase 1.
- **Ordering:** embedded ack rides app data; if the data event errors mid-frame the
  ack must still apply. Apply acks in a first pass over the frame, before RPC
  dispatch, so a handler-side error can't skip the transport ack.
- **Measurement:** the in-process loopback bench has ~0 loss, so it won't exercise
  the retransmit/interop fallback — add explicit loss-injection tests (below).

## Test & bench plan

- **Unit:** embedded `DISPATCH_STREAM_ACK` round-trips through `StreamWindow::{encode,
  decode}`; receiver applies `on_ack`/`apply_authoritative_grant`; quarantine drops
  it for a closed stream.
- **Loss injection:** with piggyback on, drop the RESPONSE packet → request stream
  must still recover (standalone grant fallback or retransmit). Drop the embedded ack
  only (peer lacks support) → standalone grant still acks.
- **Interop matrix:** {supports, doesn't} × {supports, doesn't} — suppression only
  when both support; no unacked window otherwise.
- **Streaming regression:** `nrpc_streaming.rs` green (credit cadence unchanged).
- **Throughput:** `nrpc_qps c16/32B` + `c128/32B` before/after; expect a material
  rise as recv-loop packets/call drop 2 → 1. Feed the number back into the QPS
  plan's Phase 3 table.

## Sequencing

1. Add `DISPATCH_STREAM_ACK` + receiver apply (additive, behind capability) — no
   suppression yet. Land + soak.
2. Add capability negotiation + sender emit on the response/request legs.
3. Enable suppression when both peers support. Re-bench.
4. Report results into `NRPC_QPS_CONCURRENCY_SCALING_PLAN.md` Phase 3.

## Status

| Step | State |
|---|---|
| Design (this doc) | ✅ Drafted |
| 0 — extract shared grant-apply path | ✅ Done (`ca4fc7d5c`) — `NetSession::apply_authoritative_grant_with_ack` |
| 1 — additive embedded-ack apply | ✅ Done (`d3fe1b12b`) — `DISPATCH_STREAM_ACK` + receiver apply/strip, inert until a sender emits |
| 2 — capability negotiation + sender emit | ◐ Next — the transport↔RPC seam (response-emit needs the request stream's `rx_ack_seq`) |
| 3 — suppress standalone grant | ☐ Not started |
| 4 — re-bench + report | ☐ Not started |
