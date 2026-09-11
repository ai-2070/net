# S0d — send / ingress inventory and the proposed seam contract

Stage 0 / S0d of
[`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
(§1 "The send path is part of the refactor", §2 Inbound/Outbound,
Stage 1). **Desk inventory — no code changed.** Every row cites
`file:line` at:

**Commit read: `76a8ee4965c232f9ebdbf23ef02c9c03079bfcaf`** (branch
`LZL0/webrtc-transport`). Paths are relative to
`net/crates/net/src/adapter/net/`.

Verification helper (read-only, committed under `spikes/tools/`):
`python spikes/tools/sites.py --scan <file>` lists every send site with
its enclosing `fn`; `python spikes/tools/sites.py <file> <line>…` prints
the context each row below was classified from.

---

## 1. Outbound inventory

Columns: **class** · **destination typing today** · **blocking** ·
**error mapping** · **batching** · **guard hazard** · **RTC
disposition**.

"Guard hazard" = a `DashMap`/lock guard held across the `await`. The
repo has had exactly this bug; it is now a named witness,
`org_routing_wiring_tests.rs:7784-7807`
("**HOLD item 2 — the peer shard is RELEASED before the send awaits**",
which dies if the guard is restored across the await). The rule is
documented at `mesh.rs:9309-9311`. **Every row below is `no`** — see
§4.

### 1.1 `mesh.rs` — raw socket writes

| # | site | fn | class | destination typing | blocking | error mapping | batching | guard | RTC disposition |
|---|---|---|---|---|---|---|---|---|---|
| 1 | `mesh.rs:515` | `emit_control_chunks@495` | subprotocol reply — control-plane chunks (caller-supplied `subprotocol_id`), `CONTROL_STREAM_ID` | `SocketAddr` param, snapshotted from `PeerEntry::addr()` = `PeerTransport::send_addr()` (`:3070`) | awaited, no deadline | `is_ok()` → only counts stats on success; error dropped | per-packet, loop over chunks | no (addr copied in by caller) | refuse-at-admission |
| 2 | `mesh.rs:4827` | `flood_event_pingwave_rounds@4797` | pingwave (raw UDP, unencrypted) | `SocketAddr` from a **pre-await snapshot** `Vec<SocketAddr>` of `peers.iter().map(addr())` (`:4813-4823`) | awaited, fire-and-forget `let _ =` | ignored | per-packet fan-out | no — explicit "snapshot before awaiting" (`:4811-4812`) | **drop with counter** (a leaf never originates pingwaves, §7; an anchor must not pingwave a provisional browser) |
| 3 | `mesh.rs:5070` | `run_route_withdrawal_flood@5013` | subprotocol `SUBPROTOCOL_ROUTE_WITHDRAW` | `SocketAddr` from pre-await `targets` snapshot (`:4810`, `:5058`) | awaited, `let _ =` | ignored | per-packet fan-out | no | drop with counter |
| 4 | `mesh.rs:6797` | `consume@6755` (org egress queue) | subprotocol — org sensing frame | `next.addr`, carried in the queued `EgressDatagram` (originally a peer `addr()`) | awaited under `bound_datagram_send(.., deadline)` — queue's own deadline, not `DATAGRAM_SEND_DEADLINE` | outcome recorded in `OrgEgressCounters`, never retried, "never counted as sent" (`:6745-6749`) | per-packet, single consumer | no (queue holds owned values; `queue.lock()` released before the send, `:6766-6772`) | refuse-at-admission (the queue is already bounded — this is the closest existing analogue of §2) |
| 5 | `mesh.rs:6806` | `consume@6755` | same as #4, non-test arm | same | same | same | same | no | same |
| 6 | `mesh.rs:7815` | `spawn_sensing_frame_send@7788` | subprotocol — sensing frame, spawned | `SocketAddr` param (peer `addr()`) | **spawned task**, `let _ =` | ignored | per-packet | no (moved into the task) | drop with counter |
| 7 | `mesh.rs:9317` | `send_datagram@9312` | **funnel**: any subprotocol frame sent by node id | `SocketAddr` arg, always a released snapshot | awaited under `bound_datagram_send(.., DATAGRAM_SEND_DEADLINE)` (5 s, `:233`) | `Result<(), AdapterError>` to the caller | per-packet | no — its doc comment is the rule (`:9309-9311`) | refuse-at-admission |
| 8 | `mesh.rs:24568` | `dispatch_packet@24321` | **forwarded-for-others** — pingwave re-flood | `SocketAddr` from the peer snapshot; `filter` applied | **`try_send_to`, synchronous non-blocking** | `let _ =`; comment states dropping is deliberate (`:24563-24567`) | per-packet | no | drop with counter |
| 9 | `mesh.rs:24751` | `dispatch_packet@24321` | **forwarded-for-others** — legacy routed relay | **`RouteEntry::next_hop`** via `router.routing_table().lookup(dest_id)` (`:24699`) | `try_send_to`, synchronous | `tracing::debug!` only (SEC-07 shed, `:24733-24750`) | per-packet | no | **denied for provisional peers (§12); otherwise drop with counter** |
| 10 | `mesh.rs:25057` | `relay_protected_hop@24860` | **forwarded-for-others** — authenticated subnet route-hop | `egress_addr` from the subnet gateway's egress resolution | `try_send_to`, synchronous | `tracing::debug!`; success increments `protected_relay_stats.record_forwarded()` | per-packet | no | denied for provisional; otherwise drop with counter |
| 11 | `mesh.rs:25479` | `handle_routed_handshake@25087` | handshake msg — routed msg2 reply | `next_hop` = `routing_table().lookup(peer_node_id)` **else** `source` (`:25213-25217`) — route entry *or* packet source | **spawned task**, awaited inside | `Ok` → `guard.commit()`; `Err` → registration rollback (`:25487+`) | per-packet | no (everything cloned into the task, `:25462-25477`) | refuse-at-admission (must stay synchronous-ish: the rollback is the contract) |
| 12 | `mesh.rs:25741` | `process_local_packet@25569` | subprotocol `SUBPROTOCOL_MIGRATION` | `dest_addr` from `peers.get(&msg.dest_node).addr()` snapshot (`:25715-25716`) | spawned task, `let _ =` | ignored | per-packet | no | denied pre-admission (§12 lists migration) |
| 13 | `mesh.rs:25792` | `process_local_packet@25569` | subprotocol `SUBPROTOCOL_MIGRATION` reply | same | spawned task, `let _ =` | ignored | per-packet | no | denied pre-admission |
| 14 | `mesh.rs:25906` | `process_local_packet@25569` | **retransmission** (NACK-driven) | `parsed.source` — **packet source** (`:25902`) | spawned task, awaited in a loop | `is_ok()` → `control_stats.retransmit_packets_sent` | per-packet loop | no | refuse-at-admission |
| 15 | `mesh.rs:26399` | `process_local_packet@25569` | subprotocol `SUBPROTOCOL_REFLEX` response (traversal) | `dest_addr` from peer snapshot (`:26370`) | spawned task, `let _ =` | ignored | per-packet | no | **not applicable** — UDP-only by nature (§6) |
| 16 | `mesh.rs:27187` | `spawn_stream_grant_drainer_loop@27101` | subprotocol `SUBPROTOCOL_STREAM_WINDOW` (`0x0B00`) grant | `peer_addr` snapshot | awaited | `Err` → `tracing::debug!` + `continue`; counter bumped only on success (`:27191-27196`) | one datagram per *batched* grant chunk | no | refuse-at-admission (required for bootstrap, §12) |
| 17 | `mesh.rs:27287` | `spawn_retransmit_loop@27244` | **retransmission** | `peer.value().addr()` collected into `work: Vec` **before** the sends (`:27279`) | awaited | `is_ok()` → `retransmit_packets_sent` | per-packet | no — explicit snapshot | refuse-at-admission |
| 18 | `mesh.rs:27554` | `spawn_heartbeat_loop@27440` | heartbeat | `peer_addr` from `snapshot: Vec` (`:27543-27551`) | awaited, `let _ =` | ignored | per-packet | no — explicit snapshot | defer (session maintenance; §12 permits it, but it must not run for a closed channel) |
| 19 | `mesh.rs:27556` | `spawn_heartbeat_loop@27440` | pingwave (raw UDP) | same snapshot | awaited, `let _ =` | ignored | per-packet | no | **drop with counter** for RTC peers |
| 20 | `mesh.rs:28397` | `send_routed@28342` | **routed data** (MTU-split loop arm) | `next_hop` — route/relay address resolved by the caller | awaited | `map_err → AdapterError::Connection`, `?` | per-packet inside a `Batch` split | no | refuse-at-admission |
| 21 | `mesh.rs:28425` | `send_routed@28342` | routed data (tail arm) | same | awaited | same | same | no | refuse-at-admission |
| 22 | `mesh.rs:33536` | `handle_punch_request@33474` | traversal — rendezvous rejection | `a_addr` (requester peer addr) | spawned task, `let _ =` | ignored | per-packet | no | not applicable (UDP-only) |
| 23 | `mesh.rs:33706` | `handle_punch_request@33474` | traversal — rendezvous introduce → requester | `a_addr` | spawned task, awaited | `tracing::debug!` (`:33704-33708`) | per-packet | no | not applicable |
| 24 | `mesh.rs:33726` | `handle_punch_request@33474` | traversal — rendezvous introduce → peer B | `b_addr` | spawned task, awaited | `tracing::debug!` | per-packet | no | not applicable |
| 25 | `mesh.rs:33958` | `schedule_punch@33862` | traversal — keep-alive punch train | `peer_reflex` — **reflex (server-reflexive) address**, not `PeerTransport` | spawned, awaited per offset (`sleep_until` paced) | first error kept in `first_err`, reported once | per-packet ×3 | no | not applicable |
| 26 | `mesh.rs:33992` | `schedule_punch@33862` | traversal — keep-alive punch train (second arm) | `peer_reflex` | same | same | same | no | not applicable |
| 27 | `mesh.rs:34054` | `schedule_punch@33862` | traversal — `PunchAck` to coordinator | `coord_addr` | spawned, awaited | `tracing::debug!` | per-packet | no | not applicable |
| 28 | `mesh.rs:34104` | `forward_punch_ack@34068` | traversal — forwarded `PunchAck` | `dest_addr` from peer snapshot | spawned, `let _ =` | ignored | per-packet | no | not applicable |
| 29 | `mesh.rs:34860` | `send_membership_ack@34820` | subprotocol `SUBPROTOCOL_CHANNEL_MEMBERSHIP` (`0x0A00`) Ack | `dest_addr = peer_entry.value().addr()` (`:34830`) | spawned, `let _ =` | ignored | per-packet | no | refuse-at-admission (required for bootstrap) |
| 30 | `mesh.rs:35503` | `try_publish_to_peer@35397` | **publish** (channel fan-out; also carries every nRPC frame) | `next_hop` = `routing_table().lookup(peer_node_id)` **else** `dest_addr` (`:35497-35501`) | awaited | `Err` → `PeerPublishOutcome::SendFailed(AdapterError::Connection)`; success → `guard.commit()` (tx credit) | per-packet | no (session+addr snapshot taken first, `:35389-35396`) | refuse-at-admission — **this is the row the RTC contract most has to get right**: `guard.commit()` must stay on the synchronous side |
| 31 | `mesh.rs:37543` | `send_transfer_control@37515` | **transfer control** (`SUBPROTOCOL_BLOB_TRANSFER`, `PacketFlags::RELIABLE`) | `dest_addr` (peer addr) | awaited | `map_err → AdapterError::Connection`, `?` | per-packet | no | refuse-at-admission |
| 32 | `mesh.rs:39387` | `deliver_stream_packet@39363` | **stream data** (unscheduled arm) | `peer_addr` param (from `PeerTransport::send_addr()`) | awaited | **`StreamError::Transport`** — never `Backpressure` (`:39390`) | per-packet | no | refuse-at-admission — §1's named invariant: a UDP `WouldBlock` must not become `Backpressure`; the RTC arm is the *only* one allowed to return `Backpressure` |
| 33 | `mesh.rs:39380` | `deliver_stream_packet@39363` | **stream data (scheduled arm)** — `scheduler().enqueue` | `QueuedPacket::dest: SocketAddr` (`router.rs:198`) | non-blocking enqueue | full queue → `StreamError::Backpressure` (`:39383`) | scheduler batch (see §1.3) | no | refuse-at-admission (already the right shape) |
| 34 | `mesh.rs:39566` | `try_connect_via_once@39514` | handshake msg — routed msg1 inside a `RoutingHeader` | `relay_addr` — **caller/config supplied** (`SocketAddr` argument) | awaited | `Err` → remove `pending_handshakes` entry + `AdapterError::Connection` | per-packet | no | refuse-at-admission |
| 35 | `mesh.rs:40321` | `try_handshake_initiator@40283` | handshake msg — direct msg1 | `peer_addr` param (config/caller) | awaited | `Err` → `deregister_direct_initiator` + `AdapterError::Connection` | per-packet | no | refuse-at-admission |
| 36 | `mesh.rs:40377` | `try_handshake_initiator@40283` | handshake msg — direct msg1 (second arm) | `peer_addr` | awaited | same | per-packet | no | refuse-at-admission |
| 37 | `mesh.rs:40747` | `try_handshake_responder@40596` | handshake msg — direct msg2 | **`source`, the packet source** | awaited | `map_err → AdapterError::Connection`, `?` | per-packet | no | refuse-at-admission |

### 1.2 `mesh.rs` — logical send sites that funnel through `send_datagram` (#7)

§1's raw-`send_to` list misses these; they are distinct *classes* and
Stage 1 will touch their signatures, so they are rows in their own
right.

| # | site | fn | class | destination typing | blocking | error mapping | batching | guard | RTC disposition |
|---|---|---|---|---|---|---|---|---|---|
| 38 | `mesh.rs:28302` | `send_to_peer_node@28256` | stream data / batch chunk (MTU-split loop) | peer addr snapshot taken and **copied before the loop** (`:28257-28262`) | awaited via #7 (5 s deadline) | `?` → `AdapterError` | per-chunk | no — the fix this comment records | refuse-at-admission |
| 39 | `mesh.rs:28323` | `send_to_peer_node@28256` | same, tail chunk | same | same | same | same | no | refuse-at-admission |
| 40 | `mesh.rs:33389` | `forward_scoped_announcement@33360` | forwarded-for-others — scoped capability announcement | `peer.addr` snapshot | awaited via #7, `let _ =` | ignored | per-packet fan-out | no | denied pre-admission (§12: announcement ingestion/discovery) |
| 41 | `mesh.rs:33443` | `forward_capability_announcement@33396` | forwarded-for-others — capability announcement flood | `peer.addr` snapshot | awaited via #7, `let _ =` | ignored | per-packet fan-out | no | denied pre-admission |
| 42 | `mesh.rs:35820` | `send_subprotocol_to_node@35765` | **funnel** — any subprotocol frame by node id (`0x0A00` membership requests ride here) | peer addr snapshot; guard released first (`:35771`) | awaited via #7 | `?` → `AdapterError` | per-packet | no | refuse-at-admission |

### 1.3 `mod.rs`, `proxy.rs`, `router.rs`, `transport.rs`

| # | site | fn | class | destination typing | blocking | error mapping | batching | guard | RTC disposition |
|---|---|---|---|---|---|---|---|---|---|
| 43 | `mod.rs:934` | `try_handshake_initiator@924` | handshake msg1 (single-peer adapter) | `self.config.peer_addr` — **literal config** | awaited | `map_err → AdapterError::Connection` | per-packet | no | not applicable (single-peer UDP adapter) |
| 44 | `mod.rs:1138` | `try_handshake_responder@1026` | handshake msg2 | **`source`** (`:1135-1136`) | awaited | `map_err → AdapterError::Connection` | per-packet | no | not applicable |
| 45 | `mod.rs:1407` | `spawn_heartbeat@1374` | heartbeat | `peer_addr` captured by the loop | awaited | `tracing::warn!` | per-packet | no | not applicable |
| 46 | `mod.rs:1553` | `on_batch@1499` | stream data (MTU-split arm) | `peer_addr` | awaited | `map_err → AdapterError::Connection`, `?` | per-packet in a `Batch` | no — "No DashMap lock held during this .await" (`:1551`) | not applicable |
| 47 | `mod.rs:1603` | `on_batch@1499` | stream data (tail arm) | `peer_addr` | awaited | same | same | no | not applicable |
| 48 | `proxy.rs:357` | `forward_and_send@349` | forwarded-for-others (proxy forward) | `next_hop` from the proxy route table | awaited | `match` → `ProxyError`/`ForwardResult` | per-packet | no | denied pre-admission |
| 49 | `proxy.rs:407` | `send_to@407` | primitive passthrough | caller-supplied | awaited | `io::Result` to caller | per-packet | n/a | follows its callers |
| 50 | `router.rs:909` | `send_to@909` | primitive passthrough ("send directly, bypassing routing") | caller-supplied | awaited | `io::Result` | per-packet | n/a | follows its callers |
| 51 | `router.rs:984` | `start@923` (scheduler drain, depth-0 fast path) | **stream data / forwarded** — whatever was enqueued | `QueuedPacket::dest: SocketAddr` (`:198`) | awaited, `let _ =` | ignored | single packet (fast path when `current_depth() == 0`) | no | RTC per-peer queue (see §3.3) |
| 52 | `router.rs:1028` | `start@923` (Linux tail after `sendmmsg`) | same | `*dest` of the group | awaited, `let _ =` | ignored | **`sendmmsg` group** of up to `MAX_DRAIN = 64`, grouped by dest (`:952-1007`); this arm ships the unsent tail | no | RTC per-peer queue |
| 53 | `router.rs:1034` | `start@923` (non-Linux) | same | `*dest` | awaited, `let _ =` | ignored | per-packet within the group | no | RTC per-peer queue |
| 54 | `router.rs:895` | `route_packet@769` | **forwarded-for-others** — enqueue that ends in a socket write | `next_hop` → `QueuedPacket::dest` | non-blocking `enqueue` | `false` → `RouterError::QueueFull` + `packets_dropped` / `record_drop` | scheduler | no | denied pre-admission for provisional peers; otherwise RTC queue |
| 55 | `transport.rs:259/265/280` | `NetSocket::{send_to, send, try_send_to}` | primitives | caller-supplied | awaited / sync | `io::Result` | per-packet | n/a | **these are the functions the seam replaces** |
| 56 | `transport.rs:699/705/711` | `PacketSender::{send_to, send, try_send_to}` | primitives | caller-supplied | awaited / sync | `io::Result` | per-packet (plus `send_batch` on Linux) | n/a | same |

### 1.4 Out of scope by nature

`traversal/portmap/natpmp.rs:900`, `:1021`, `:1065`, `:1089` (and the
test-only `:870`, `:1194`) send NAT-PMP/PCP requests to the **gateway**
on their own socket. They are UDP-by-definition infrastructure probes,
never peer-addressed, and §6 keeps traversal UDP-only. No `PeerAddr`,
no RTC disposition. `linux.rs:908` is test-only.

## 2. Ingress inventory

| # | entry point | source typing | ordering | `ConnectionReset` | single dispatch owner? |
|---|---|---|---|---|---|
| I1 | `mesh.rs:24280` `IngressReceiver::Single` → `PacketReceiver::recv()` (`transport.rs`) → `dispatch_packet(data, source, ctx)` (`:24283`) | `SocketAddr` from `recv_from` | per-socket arrival order, one packet at a time | **transient** — logged and tolerated (`reset_is_fatal() == false`, `:24243`, `:24300-24304`). This is the Windows ICMP-port-unreachable case S0b §7.4 hit. | **yes** — the one owner |
| I2 | `mesh.rs:24280` `IngressReceiver::Batched` → `BatchedPacketReceiver::recv()` (Linux + `batched-ingress`) | `SocketAddr` per packet from `recvmmsg` | same `recv()` contract, "one packet at a time, arrival order" (`:24216-24217`) | **fatal** — the backing thread has exited, loop breaks rather than busy-spins (`:24233-24246`, `:24289-24299`) | yes — same loop |
| I3 | `mesh.rs:24340` keep-alive recognition inside `dispatch_packet` | `SocketAddr` (`source`) matched against `ctx.punch_observers` | before any session decrypt; returns early | n/a (no recv of its own) | yes |
| I4 | `mesh.rs:24602` `relay_protected_hop(&data, ctx)` — route-hop envelope branch | `source` + the hop's own authentication | inside dispatch | n/a | yes |
| I5 | `mesh.rs:24614` routed-envelope **local delivery** → `handle_routed_handshake` (`:24632`) or `process_local_packet` (`:24672`) | inner packet's `session_id`, **not** the source address (`:24635-24670`); `source` still carried on `ParsedPacket` | inside dispatch | n/a | yes |
| I6 | `mesh.rs:24675` routed-envelope **forward** branch | `RouteEntry::next_hop` | inside dispatch | n/a | yes |
| I7 | `mesh.rs:40398` `socket_arc.recv_from(..)` inside `try_handshake_initiator`'s `select!` | `SocketAddr` from `recv_from` | **competes with I1 for datagrams on the same socket** — a bypass, not a branch of the dispatch owner | error → `AdapterError::Connection` and the handshake fails | **no** |
| I8 | `mesh.rs:40649` `socket_arc.recv_from(..)` inside `try_handshake_responder` | same | same bypass | same | **no** |
| I9 | `mesh.rs` `DirectHandshakeInbox` (`:577`, registry `:679`, `pending_direct_initiators` `:1565`) — **synthetic injection**: `dispatch_packet` hands a handshake payload to the waiting initiator instead of the socket loop | keyed by `SocketAddr` | delivered from the dispatch owner into a per-address inbox | n/a | yes (producer side) / no (consumer side) |
| I10 | `mod.rs:957`, `mod.rs:1062` `recv_from` in the single-peer adapter's handshake paths | `SocketAddr` | its own loops; this adapter has no `dispatch_packet` | error → `AdapterError` | n/a (separate adapter) |
| I11 | `proxy.rs:412` `Proxy::recv_from`, `router.rs:914` `Router::recv_from` | `SocketAddr` | primitives with no loop of their own in production | `io::Result` to caller | n/a |
| I12 | `traversal/portmap/natpmp.rs:867`, `:905` | gateway `SocketAddr` | own socket, own loop | `io::Result` | n/a — not mesh ingress |

**The bypasses matter for Stage 3.** I7/I8 read the *same* UDP socket as
I1 while a handshake is outstanding: they are the reason
`pending_direct_initiators`/`DirectHandshakeInbox` (I9) exist at all.
An RTC endpoint has no `recv_from` to steal from, so on the RTC path
these two must be served exclusively by the I9 inbox shape.

## 3. Proposed seam contract

Not code — the shape Stage 1 implements and Stage 3 extends.

### 3.1 UDP-preserving submission (Stage 1, behaviour-neutral)

```rust
// One submission surface, transport-specific behaviour behind it.
impl PeerSink {
    /// Awaited submission. UDP: exactly today's `socket.send_to(..).await`.
    async fn send(&self, packet: &[u8], to: PeerAddr) -> io::Result<usize>;

    /// Non-blocking submission. UDP: exactly today's `try_send_to`.
    fn try_send(&self, packet: &[u8], to: PeerAddr) -> io::Result<usize>;

    /// Awaited submission under a caller-chosen deadline. UDP: today's
    /// `bound_datagram_send(socket.send_to(..), addr, deadline)`.
    async fn send_bounded(&self, packet: &[u8], to: PeerAddr, deadline: Duration)
        -> Result<(), AdapterError>;
}
```

Three entry points because the inventory has exactly three blocking
shapes, and collapsing them would change behaviour:

- `send` serves rows 1–3, 6, 11–32, 34–37, 43–48, 51–53 (and the
  primitives 49/50/55/56);
- `try_send` serves rows 8, 9, 10 — the three **deliberate shed**
  sites, whose comments (`:24563-24567`, `:24733-24750`, `:25049-25056`)
  explicitly reject queuing;
- `send_bounded` serves rows 4, 5, 7 (and through #7, rows 38–42).

**Stage 1's exit criterion, stated as a property of this table:** after
the edit, every row's *blocking*, *error mapping* and *batching* column
is unchanged, and no row's guard column becomes `yes`. Concretely:
row 32 still maps failure to `StreamError::Transport` and row 33 is
still the only `Backpressure` producer; rows 4/5/7 still carry their
deadlines (the org-egress queue's own, and `DATAGRAM_SEND_DEADLINE` =
5 s, `mesh.rs:233`); rows 8/9/10 remain synchronous sheds; rows 51–53
still group by destination for `sendmmsg`.

### 3.2 RTC submission (Stage 3)

Per §2, unchanged in shape by this inventory:

```rust
/// RTC half. `WouldBlock` == `Backpressure`. Synchronous and total.
fn try_send(&self, packet: &[u8], to: RtcPeerId) -> io::Result<()>;
```

- **Hard bound**: reserved queue slots/bytes (`send_queue_packets`,
  default 256), accounted at admission. S0b measured this working:
  2 343 admission refusals with zero in-flight loss under a saturated
  channel.
- **Advisory input**: the driver-published `buffered_amount` snapshot —
  **and S0b showed the plan's 256 KiB default is unreachable**, because
  str0m caps at `MAX_BUFFERED_ACROSS_STREAMS = 128 KiB` across all
  streams. Stage 3 must set the advisory below 128 KiB or the input is
  dead code.
- **Rows it serves**, from the table above: every row marked
  *refuse-at-admission* (1, 4–7, 11, 14, 16, 17, 20, 21, 29–39, 42),
  with row 30 (`try_publish_to_peer`) and row 32
  (`deliver_stream_packet`) as the two that carry a **credit commit**
  and therefore fix where the synchronous boundary is: `guard.commit()`
  (`mesh.rs:35509`) and the retransmit registration must both happen
  after a *completed* admission decision, never after a deferred one.
- Rows marked *drop with counter* (2, 3, 6, 8, 9, 10, 19, 40, 41) get a
  named counter each; rows marked *not applicable* (15, 22–28, 43–47)
  stay UDP-only.

### 3.3 Scheduler drain split

`QueuedPacket::dest: SocketAddr` (`router.rs:198`) becomes `PeerAddr`.
The drain (`router.rs:923-1083`) keeps its current structure and gains
one partition step:

- `PeerAddr::Udp` packets are grouped by destination exactly as today
  (`reset_dest_groups` / `group_by_dest`, `:991-1007`) and flushed with
  `sendmmsg` on Linux (`:1026`) with the async tail send (`:1028`),
  per-packet elsewhere (`:1034`);
- `PeerAddr::Rtc` packets are **not** batched — they are submitted one
  at a time to the driver's per-peer queue, because str0m's contract is
  one `Channel::write` per `poll_output` drain (S0b §2). The depth-0
  fast path (`:982-987`) applies to both.
- The `MAX_DRAIN = 64` bound and the drain-run instrumentation stay
  whole-drain, not per-transport, so the existing measurements remain
  comparable.

### 3.4 Bounded RTC ingress on `IngressReceiver`

`IngressReceiver` (`mesh.rs:24218`) gains a third variant fed by a
**bounded** channel the RTC driver pushes into; `recv()` keeps its
contract ("one packet at a time, arrival order", `:24216-24217`) and
`dispatch_packet` stays the single owner (I1–I6 unchanged).

- **Ordering.** Per-source ordering is preserved *within* each input,
  never across them: UDP arrival order is the socket's, RTC arrival
  order is the driver's per-channel order. The `select!` at `:24279`
  interleaves the two inputs fairly, which is sound because a Net
  session belongs to exactly one endpoint at a time — there is no
  packet stream that can split across both inputs. (When a peer
  migrates routed→direct or UDP→RTC, the existing stale-session and
  generation protections already cover the crossover; §1 requires them
  re-exercised.)
- **`ConnectionReset`.** Two different meanings, and the existing
  `reset_is_fatal()` (`:24241`) is exactly the right place to encode
  the third: for the **RTC UDP socket** a `ConnectionReset` is an ICMP
  port-unreachable about a peer that went away and **must be swallowed**
  (S0b §7.4 — treating it as fatal kills every session on that socket);
  it is *not* routed into the ingress input at all, it is handled
  inside the driver. A `ConnectionReset`-equivalent on the RTC *input*
  (the driver's sender dropped) means the driver died: fatal, break,
  like the batched receiver.
- **Backpressure.** A full RTC ingress input drops with a counter
  (S0b's harness does exactly this) — it never blocks the driver,
  because blocking the driver stalls every peer's `poll_output`.

### 3.5 Rows where the destination is NOT `PeerTransport` — `PeerAddr`'s leak surface

Stage 1 must budget for these; they are where `PeerAddr` crosses out of
`mesh.rs`'s peer table.

**From a route entry (`RouteEntry::next_hop`, `route.rs`) — 5 rows:**
row 9 (`mesh.rs:24699`), row 11 (`:25213`, falls back to `source`),
row 30 (`:35497`), row 54 (`router.rs:889`), and the routed-forward
lookup feeding rows 51–53 through `QueuedPacket::dest`.

**From a packet source — 5 rows:** row 11's fallback (`:25217`),
row 14 (`:25902`), row 37 (`mesh.rs:40747`), row 44 (`mod.rs:1138`),
and I9's inbox keying (`pending_direct_initiators`, `SocketAddr`-keyed,
`:679`).

**From a reflex / traversal address — 2 rows:** rows 25 and 26
(`peer_reflex`). These stay `SocketAddr` (§6).

**From literal config — 1 row:** row 43 (`mod.rs:934`,
`config.peer_addr`), which §1 explicitly keeps as `SocketAddr`.

So: **10 rows** force `PeerAddr` into `route.rs` / the source-derived
paths, and `reroute.rs` / `failure.rs` follow them because they key on
the same `next_hop` / peer-addr values. That is the count Stage 1 should
plan against — not the 30-odd raw send sites, which are mechanical.

## 4. Counts

**Outbound sites: 56 rows** (49 production call sites + 7 primitive
functions). By class:

| class | rows | count |
|---|---|---|
| stream data | 32, 33, 38, 39, 46, 47 | 6 |
| routed data | 20, 21 | 2 |
| forwarded-for-others | 8, 9, 10, 40, 41, 48, 54 (+51–53 as the drain that ships them) | 7 (+3 drain) |
| retransmission | 14, 17 | 2 |
| heartbeat | 18, 45 | 2 |
| pingwave | 2, 19 | 2 |
| subprotocol reply (by id) | 1 (control), 3 (`ROUTE_WITHDRAW`), 4/5/6 (org sensing), 12/13 (`MIGRATION`), 16 (`0x0B00`), 29 (`0x0A00`), 31 (`BLOB_TRANSFER`) | 10 |
| handshake msg | 11, 34, 35, 36, 37, 43, 44 | 7 |
| traversal (reflex / rendezvous / keep-alive) | 15, 22, 23, 24, 25, 26, 27, 28 | 8 |
| publish | 30 | 1 |
| funnels / primitives | 7, 42, 49, 50, 55, 56 | 6 (7 fns) |

By blocking semantics:

| semantics | rows | count |
|---|---|---|
| awaited, error propagated (`?` / typed) | 20, 21, 30, 31, 32, 34, 35, 36, 37, 38, 39, 42, 43, 44, 46, 47, 48 | 17 |
| awaited, fire-and-forget (`let _ =` / `is_ok()`) | 1, 2, 3, 12, 13, 14, 15, 16, 17, 18, 19, 22–28, 29, 40, 41, 45, 51, 52, 53 | 25 |
| awaited under a deadline (`bound_datagram_send`) | 4, 5, 7 | 3 |
| synchronous non-blocking (`try_send_to`) | 8, 9, 10 | 3 |
| scheduler-queued (non-blocking enqueue) | 33, 54 | 2 |
| spawned task around the send | 6, 11, 12, 13, 14, 15, 22–28, 29 | 14 (overlaps the fire-and-forget row) |

**Ingress entries: 12** (I1–I12), of which **6** (I1–I6) run on the
single dispatch owner, **4** are bypasses or separate adapters
(I7, I8, I10, I11), **1** is synthetic injection (I9) and **1** is
non-mesh (I12).

**Sites holding a guard across an await: 0.** Verified by reading each
awaiting row's pre-await context: the fan-out sites snapshot into a
`Vec` first (`:4811-4823`, `:5058`, `:27279`, `:27543-27551`), the
peer-keyed funnels copy-and-release (`:28257-28262`, `:35389-35396`,
`:35771`), the spawned sites move owned clones into the task
(`:25462-25477`), and the org-egress consumer drops `queue.lock()`
before sending (`:6766-6772`). The rule is documented at `:9309-9311`
and enforced by `org_routing_wiring_tests.rs:7807`
(`a_send_in_flight_retains_no_peer_shard`). **Expected zero, found
zero.**

## 5. What did not go cleanly

- **§1's site list is stale and incomplete, in both directions.** Of the
  ~30 `mesh.rs` line numbers §1 cites, the ones that still land on a
  send at this commit are `:515`, `:4827`, `:5070`, `:6797`/`:6806`,
  `:7815`, `:9317`, `:25479`, `:25741`, `:25792`, `:25906`, `:26399`,
  `:27187`, `:27287`, `:27554`/`:27556`, `:28397`/`:28425`, `:33536`,
  `:34104`, `:34860`, `:35503`, `:37543`, `:39387`, `:39566`, `:40321`,
  `:40377`. **Missing from §1:** `:24568`, `:24751`, `:25057` (the three
  `try_send_to` shed sites — a different blocking class, not just more
  rows), `:33706`, `:33726`, `:33958`, `:33992`, `:34054`, `:40747`, and
  the five `send_datagram` funnel callers (`:28302`, `:28323`, `:33389`,
  `:33443`, `:35820`). §1 also says "five in `mod.rs`" — there are
  exactly five, which checks out. The count Stage 1 should carry is
  **49 call sites**, not ~36.
- **The `proxy.rs:357` citation in §1 is right, but `proxy.rs` is a
  separate forwarder with its own route table**, not the mesh router.
  Nothing in the plan says whether the proxy participates in `PeerAddr`
  at all. Left classified as "denied pre-admission" because a
  provisional browser must not reach it; Stage 1 should decide
  explicitly whether `Proxy` is in scope or frozen as UDP-only.
- **`router.rs:198` `QueuedPacket::dest` is cited by §1; `router.rs:895`
  (`route_packet`'s enqueue) is not.** That is the site that *creates*
  forwarded-for-others queue entries from a `RouteEntry::next_hop`, so
  it is where `PeerAddr` enters the scheduler. It is a row here (54).
- **Two sites resist a clean class.** Row 11
  (`handle_routed_handshake`'s msg2) is a handshake message whose
  destination is a route entry *or* the packet source, chosen at
  runtime (`:25213-25217`) — it belongs to both "handshake msg" and the
  route-derived leak surface. Row 30 (`try_publish_to_peer`) is listed
  as "publish", but it is also the transport for **every nRPC frame**
  (S0e), so its RTC disposition governs far more than channel fan-out.
- **"Spawned task" is not a separate blocking semantics so much as a
  hiding place.** 14 rows spawn a task and then await inside it. The
  await's deadline, error handling and ordering all still exist — they
  are just detached from the caller, and in row 11's case a rollback
  guard travels into the task to compensate. Any seam that changes
  submission timing has to preserve *that* structure, not just the
  `send_to` call. §1 does not mention it.
- **`bound_datagram_send`'s two deadlines are different things.** Rows
  4/5 use the org-egress queue's own `deadline` variable; only row 7
  uses the 5 s `DATAGRAM_SEND_DEADLINE` (`:233`). §1 cites
  `:6797`/`:6806` *as* `DATAGRAM_SEND_DEADLINE` sites — they are not.
- **`transport.rs`'s `send()` (connected-socket) variants (`:265`,
  `:705`) have no production caller in `adapter/net`** at this commit.
  They are part of the primitive surface the seam replaces, so they are
  listed, but no row depends on them; if Stage 1 wants a smaller seam,
  they can be dropped rather than generalized.
- **The guard-across-await audit is a read, not a proof.** Zero is what
  the code shows at this commit and what the witness enforces for the
  two funnels it names (`send_subprotocol_to_node`, `send_to_peer_node`).
  There is no mechanical check that a *new* site cannot reintroduce it;
  the witness covers those two paths only.
