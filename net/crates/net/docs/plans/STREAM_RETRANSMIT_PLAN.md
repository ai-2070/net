# Stream retransmit — wiring reliable-stream loss recovery

**Status:** DONE (commit `78bfc4ee2`). `MeshNode` reliable streams now recover lost packets. Cross-cutting: benefits every reliable stream (nRPC streaming, blob transfer, …).

**Outcome:** D-1..D-4 wired + two receiver-reliability fixes (create-reliable-on-RELIABLE-flag; requester opens its receive stream reliable; `serve_chunk` waits for ack before close). Verified in `tests/transfer_concurrency.rs`: mid-stream loss recovers via NACK; tail loss recovers via the timeout loop; the concurrent-4 MiB sweep now passes k=2..8 (retransmit also recovers recv-buffer-overflow drops). 3307 lib tests + transfer/dir/fairness/scheduled/nrpc-mesh suites green. The `mod.rs` legacy send path was left as-is.

## The gap (verified 2026-05-31)

`MeshNode` reliable streams today provide dedup + in-order accounting + flow control, but **do not retransmit lost packets** — a dropped packet is a permanent gap → the receiver stalls to the 30 s transfer timeout. On localhost/LAN this is invisible (bounded in-flight ⇒ no drops); on a lossy link it breaks.

The machinery exists but is unwired on the MeshNode path:
- `ReliableStream::{on_send, on_nack, get_timed_out, build_nack}` are implemented; `on_nack`/`get_timed_out`/`build_nack` are called **only in `reliability.rs` tests**.
- `NackPayload` has a 16-byte wire codec (`protocol.rs`).
- **`MeshNode::send_on_stream` (mesh.rs) never calls `on_send`** — so descriptors aren't even registered (the only `on_send` is in a separate `mod.rs` send path that `MeshNode` doesn't use).
- There is no `spawn_retransmit_loop`; the receive path never emits a NACK.

So four pieces are missing on the MeshNode path: register-on-send, NACK-on-gap (receiver), resend-on-NACK (sender), and a timeout backstop.

## Design

Reuse the existing `NackPayload` wire form and the per-mesh drainer pattern that `StreamWindow` grants already use. A new `SUBPROTOCOL_STREAM_NACK` carries the NACK packet (receiver → sender), parallel to `SUBPROTOCOL_STREAM_WINDOW`.

- **Descriptors** are bounded by the reliability window (`max_pending`, default 32). With the transfer window already coupled to `max_pending`, ≤32 descriptors/stream are held (~256 KiB for an 8 KiB-frame stream) — bounded; no per-stream prealloc blow-up.

## Stages

### D-1 — Register descriptors on send (sender)
`MeshNode::send_on_stream`: after each packet is delivered (direct or scheduled), if the stream is reliable, build a `RetransmitDescriptor { seq, stream_id, events, flags }` and `stream.with_reliability(|r| r.on_send(desc))`. Mirrors the mod.rs path. Foundation for everything else.

### D-2 — Emit a NACK on gap (receiver)
On the receive path, after `on_receive`, if the stream has gaps (`build_nack` → `Some`), enqueue a NACK for a per-mesh **nack drainer** (coalesced per `(session, stream)`, latest-wins, like grants), which sends one `SUBPROTOCOL_STREAM_NACK` packet per stream per drain cycle. Gaps surface whenever an out-of-order packet is accepted, so later arrivals trigger recovery of earlier holes.

### D-3 — Resend on NACK (sender)
Dispatch `SUBPROTOCOL_STREAM_NACK`: decode `NackPayload`, resolve the peer's stream, `on_nack(&nack)` → `Vec<Arc<RetransmitDescriptor>>`, rebuild each via the session's `PacketBuilder` (fresh AEAD counter, same seq) and send it (honoring the stream's `scheduled` flag — reuse `deliver_stream_packet`).

### D-4 — Timeout backstop (sender)
`spawn_retransmit_loop`: every ~RTO, walk active reliable streams, `get_timed_out()` → resend (same rebuild path as D-3). Recovers the **tail** (last packets lost ⇒ no later arrival to trigger a NACK) and NACK-loss cases. `max_retries` (default 3) bounds attempts.

### D-5 — Lossy-path test
A deterministic drop hook (test-only config: drop every Nth outbound data packet in the router send loop) + a test that transfers a multi-packet chunk under, say, 1-in-10 loss and asserts byte-for-byte completion — proving recovery. Also a tail-loss variant (drop the last packet) to exercise D-4.

## Order
D-1 → D-2 → D-3 → verify NACK-driven recovery under mid-stream loss → D-4 → verify tail-loss recovery → confirm existing suites (blob_transfer/dir/fairness/concurrency, nRPC) unchanged.

## Out of scope / non-goals
- No change to the `NackPayload` wire form or the SACK/`next_expected` accounting.
- No FIN+ack close — once retransmit works, the existing close is fine for the common case; an ack-tracked close is a separate, smaller refinement.
- The `mod.rs` legacy send path is left as-is.
