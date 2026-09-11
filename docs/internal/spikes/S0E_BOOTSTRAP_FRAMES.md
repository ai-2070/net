# S0e — bootstrap frame inventory and the §12 allow-list

Stage 0 / S0e of
[`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
(§5 Layer 0, §12 browser admission contract, Stage 4).
**Desk inventory — no code changed.**

**Commit read: `76a8ee4965c232f9ebdbf23ef02c9c03079bfcaf`** (branch
`LZL0/webrtc-transport`). Paths relative to `net/crates/net/`.

Traced: `Mesh::join` (`sdk/src/mesh_enroll.rs:276`) from the invite
string to the verified `DelegationChain`.

---

## 1. Frame table

Abbreviations: **D→A** device→anchor, **A→D** anchor→device. "Outer"
is the datagram's outermost format. `0x0A00` =
`SUBPROTOCOL_CHANNEL_MEMBERSHIP` (`src/adapter/net/channel/membership.rs:14`),
`0x0B00` = `SUBPROTOCOL_STREAM_WINDOW`
(`src/adapter/net/subprotocol/stream_window.rs:27`).

| # | step | dir | outer format | subprotocol + **action** | stream / reliability / needs `0x0B00`? | sent at | handled at | **required?** |
|---|---|---|---|---|---|---|---|---|
| 1 | `connect_via` routed handshake **msg1** | D→A | **routing envelope** (`RoutingHeader` 18 B) wrapping a Net handshake packet (`build_handshake`, `PacketFlags::HANDSHAKE`) | none (handshake flag) | no stream; unreliable; no | `sdk/src/mesh_enroll.rs:290` → `sdk/src/mesh.rs:671-682` → core `try_connect_via_once`, `src/adapter/net/mesh.rs:39562-39566` | `mesh.rs:24594` (discriminate) → `:24614` (dest is us) → `:24631-24633` → `handle_routed_handshake`, `:25087` | **required** |
| 2 | routed handshake **msg2** | A→D | routing envelope wrapping a Net handshake packet | none | no stream; unreliable; no | `mesh.rs:25202-25206` (reply header) → send `:25479` (spawned; `guard.commit()` on success `:25485`) | device's dispatch loop → `pending_handshakes` oneshot (`:39567`, awaited `:39572`) | **required** |
| 3 | reply-channel **Subscribe** — `net.mesh.enroll.replies.<device_origin:016x>` | D→A | Net packet, subprotocol frame | `0x0A00` **`MembershipMsg::Subscribe { channel, nonce, token: None, queue_group: None }`** (`mesh.rs:29662-29675`) | `CONTROL_STREAM_ID` via `send_subprotocol_to_node`; **fire-and-forget UDP, no NACK/retransmit** (`:29683-29686`); retried up to `membership_max_attempts` reusing the same nonce (`:29695-29703`); no | `mesh_rpc.rs:5462-5468` (`ensure_reply_subscription`) → `mesh_rpc.rs:5844-5846` → `mesh.rs:29582` → `:29634` → `send_subprotocol_to_node` `mesh.rs:35765` → `send_datagram` `:35820` → `:9317` | `mesh.rs:29775` `handle_membership_message` → `MembershipMsg::Subscribe` arm `:29780`, `authorize_subscribe` `:29794` | **required** (§12 explicitly permits "the authenticated caller's reply channel") |
| 4 | membership **Ack** | A→D | Net packet, subprotocol frame | `0x0A00` **`MembershipMsg::Ack { nonce, accepted, reason }`** | `stream_id` per membership; fire-and-forget; no | `send_membership_ack` `mesh.rs:34820`, send `:34860` | device: `pending_membership_acks` oneshot (`mesh.rs:29680-29681`) | **required** |
| 5 | nRPC **REQUEST** — `net.mesh.enroll` | D→A | Net packet, published direct-to-peer (channel hash stamped on the wire) | nRPC: `EventMeta(DISPATCH_RPC_REQUEST)` + `RpcRouteV1(request_channel_hash)` + `RpcRequestPayload { service: "net.mesh.enroll", body: JoinRequest JSON, deadline_ns, flags, headers }`. **Service** `net.mesh.enroll` (`sdk/src/mesh_enroll.rs:38`), **method** = the service's single unary entry, **unary**, reply channel `net.mesh.enroll.replies.<device_origin>` | `route.request_stream_id`; **`reliable = true`** (`mesh_rpc.rs:5519`); tx credit charged at `mesh.rs:35429-35433` (`try_acquire_tx_credit_guard`), committed at `:35509` → **a `0x0B00` grant is required only if the request exceeds the initial window**; a `JoinRequest` does not | `sdk/src/mesh_enroll.rs:295-303` → `mesh_rpc.rs:5514-5522` (`publish_to_peer`) → `mesh.rs:35364` → `try_publish_to_peer` `:35397` → send `:35503` | anchor: dispatch → per-channel-hash nRPC dispatcher → handler registered by `serve_rpc_typed(ENROLLMENT_SERVICE, …)` `sdk/src/mesh_enroll.rs:175` / `:203` | **required** |
| 6 | nRPC **RESPONSE** — `JoinOutcome` | A→D | Net packet, published to the reply channel | nRPC RESPONSE frame on `net.mesh.enroll.replies.<device_origin>` | `reply_stream_id`; **`reliable = true`** (`mesh_rpc.rs:2945`); same credit rule as #5 | `mesh_rpc.rs:2901` `publish_response_to_caller` → `try_publish_to_peer` `:2941` | device: `RpcClientFold` on the subscribed reply channel → pending oneshot → `sdk/src/mesh_enroll.rs:305` | **required** |
| 7 | nRPC **CANCEL** (only on deadline expiry or future drop) | D→A | Net packet, published direct-to-peer | nRPC CANCEL, meta-only (`DISPATCH_RPC_CANCEL`), rides the request channel hash | request stream; `reliable = true` (`mesh_rpc.rs:3055`) | `mesh_rpc.rs:3039-3059` | anchor's nRPC dispatcher | **required** (§12: "its corresponding response / error / cancellation") |
| 8 | stream-window **grant** | A→D | Net packet, subprotocol frame | `0x0B00` grant for the request stream | control stream; fire-and-forget; batched per chunk | `spawn_stream_grant_drainer_loop` `mesh.rs:27101`, send `:27187` | device's `0x0B00` arm → `StreamState` credit | **required but conditional** — only emitted once the anchor consumes bytes on a credited stream; not on the critical path for a single small `JoinRequest` |
| 9 | reliable-stream **retransmit / NACK** | both | Net packet | reliability control (`PacketFlags::RELIABLE` packets + NACK-driven resend) | the bootstrap streams only | `mesh.rs:27287` (timer-driven), `:25906` (NACK-driven) | `reliability.rs` per-stream state | **required** (both #5 and #6 are `reliable = true`) |
| 10 | **heartbeat** | both | Net packet, heartbeat flag | none | no stream; fire-and-forget | `spawn_heartbeat_loop` `mesh.rs:27440`, send `:27554` | failure detector | **incidental-but-permitted** — §12 allows "necessary session maintenance"; it is not part of the enrollment exchange |
| 11 | **pingwave** | both | **raw UDP, unencrypted** (no Net header) | none — topology beacon | none | `mesh.rs:27556` (heartbeat tick) and `:4827` (event-driven flood) | `dispatch_packet` pingwave arm, re-flooded at `:24568` | **incidental** |
| 12 | **capability announcement** (periodic re-announce) | D→A and flooded onward | Net packet, subprotocol frame | `0x0C00` `SUBPROTOCOL_CAPABILITY_ANN` (`src/adapter/net/behavior/broadcast.rs:14`) | control; fire-and-forget | re-announce loop spawned by `start()` at `mesh.rs:22410` (`spawn_capability_reannounce_loop`, `:23476`); forwarded on by `:33443` / `:33389` | announcement ingestion → route learning | **incidental** |
| 13 | **corrective capability re-announce** on a rejected reply-channel Subscribe | D→A | as #12, but **bypassing the announce rate limit** | `0x0C00` | control | triggered inside `ensure_reply_subscription`'s retry (`mesh_rpc.rs:5842-5870`, "fires a rate-limit-bypassing corrective re-announce", `:29577-29581`) | as #12 | **incidental — and the sharpest one** (see §3) |
| 14 | route-withdrawal flood, sensing frames, fold traffic, migration, `0x0D02` RTC signalling | both | various | `SUBPROTOCOL_ROUTE_WITHDRAW`, org sensing, `0x1000` fold, `SUBPROTOCOL_MIGRATION`, `0x0D02` | various | `mesh.rs:5070`, `:6797`/`:6806`, `:7815`, `:25741`/`:25792` | various | **incidental** — none is reached by `Mesh::join`; listed because a started node emits them and §12 denies them pre-admission |

**Note on #1's prerequisite.** `Mesh::join`'s doc requires the node to
be `start()`ed (`sdk/src/mesh_enroll.rs:273-274`), and the SDK's
`start()` is `start_arc` (`sdk/src/mesh.rs:690-695`), which runs
`MeshNode::start` (receive loop, heartbeat loop, grant drainer,
retransmit loop, router, capability GC, **capability re-announce**;
`mesh.rs:22389-22410`) plus the direct-upgrade loop (`:22524`). Rows
10–13 are consequences of that requirement, not of the enrollment
exchange.

## 2. The §12 action-level allow-list

Derived **only** from rows 1–9 (the required ones). A provisional
session — one whose Noise handshake completed but whose enrollment has
not — may exercise exactly this set and nothing else.

```
PROVISIONAL ALLOW-LIST  (browser-facing anchor, v1)

A. Session establishment
   A1. outer: routing envelope, dest_id == this anchor's node_id,
       inner: Net packet with the HANDSHAKE flag  (msg1)
       — local delivery only; never transit (see §4)
   A2. outer: routing envelope, inner Net handshake packet  (msg2, ours)
   A3. direct Net packets on the established session_id
   bounds: 1 live provisional session per RtcPeerId; <= 3 handshake
           attempts; handshake completes within the existing
           `handshake_timeout`; no route installation from A1/A2.

B. Enrollment nRPC — one bounded unary call
   B1. subprotocol: nRPC REQUEST
       service:  exactly "net.mesh.enroll"   (sdk/src/mesh_enroll.rs:38)
       shape:    unary; reply channel exactly
                 "net.mesh.enroll.replies.<caller_origin:016x>"
       target:   this anchor only (target_node_id == local_node_id)
   B2. subprotocol: nRPC RESPONSE on that reply channel (ours)
   B3. subprotocol: nRPC CANCEL for an in-flight call_id of B1
   bounds: <= 1 in-flight enrollment call per provisional session;
           <= 4 total REQUEST frames (initial + 3 retries);
           request body <= 16 KiB;
           deadline <= 10 s (RtcConfig::ice_deadline order);
           no other service name, no streaming, no call_service
           (capability-index) routing.

C. Channel membership — 0x0A00, two actions only
   C1. Subscribe { channel == "net.mesh.enroll.replies.<caller_origin>",
                   token: None, queue_group: None }
   C2. Unsubscribe { that same channel }   (teardown)
       (Ack is ours: mesh.rs:34820)
   bounds: <= 1 channel membership per provisional session;
           <= `membership_max_attempts` Subscribe frames sharing one
           nonce (mesh.rs:29695-29703);
           wildcard channels, caller-chosen reply destinations,
           queue groups and tokens are REFUSED, not ignored.

D. Stream-window / reliability control for B's streams only
   D1. 0x0B00 grants we emit for the request stream  (mesh.rs:27187)
   D2. NACK / retransmit for packets on the request and reply streams
       (mesh.rs:25906, :27287)
   bounds: <= 2 streams (request + reply) per provisional session;
           <= 64 KiB total tracked bytes;
           no caller-initiated stream creation outside B's two.

E. Session maintenance
   E1. heartbeat, in both directions  (mesh.rs:27554)
   bounds: existing heartbeat cadence; no pingwave (row 11) to or from
           a provisional peer.

WHOLE-SESSION BOUNDS
   deadline:            provisional state expires 30 s after the
                        handshake completes, enrolled or not
   max frames:          256 inbound frames total
   max bytes:           256 KiB inbound total
   max streams:         2
   max channel members: 1
   on breach:           close the session and reclaim (§12 step 5)

EVERYTHING ELSE IS DENIED BEFORE EFFECTS
   capability announcement ingestion or emission (0x0C00, rows 12/13),
   pingwave (row 11), fold (0x1000), sensing, migration, route
   withdrawal, 0x0D02 RTC signalling, general publish/subscribe, any
   nRPC service other than net.mesh.enroll, net.mesh.renew (see §5),
   and **any forwarding whatsoever** (§4).
```

Note the shape: **B is an action allow-list, not "allow nRPC"** — the
carrier is the same for the enrollment call and for every other RPC in
the mesh, so the check has to be on `(service, unary, reply channel,
target == self)`, decoded within bounds *before* the handler runs (§12
step 3).

## 3. Incidental work found, and what to do with it

| # | incidental work | why it happens | recommended handling |
|---|---|---|---|
| 13 | **Corrective capability re-announce on a rejected reply-channel Subscribe** (`mesh_rpc.rs:5842-5870`, rationale `mesh.rs:29577-29581`) — deliberately **bypasses the announce rate limit** | `ensure_reply_subscription` treats an origin-binding `Unauthorized` as "the peer hasn't seen my capabilities yet" and re-broadcasts to fix it | **Remove from the provisional path.** A provisional session's Subscribe rejection is a *policy* answer, not a stale-announcement problem. Left in place it hands an unenrolled browser a rate-limit-bypassing broadcast trigger — one refused Subscribe per retry, each causing a mesh-wide flood. This is the single most load-bearing finding in S0e. |
| 12 | **Periodic capability re-announce** started by `start_arc` (`mesh.rs:22410` → `:23476`) | `Mesh::join` requires a started node; started nodes announce | **Defer until admitted** on the anchor side (do not ingest a provisional peer's announcement, do not flood it) and **constrain on the leaf side** (§7 already says a leaf tags `leaf` and never re-floods). The device announcing to an anchor that discards it is wasted but harmless; the anchor *ingesting and flooding* it is route installation for an unadmitted peer. |
| 11 | **Pingwave**, on every heartbeat tick (`mesh.rs:27556`) and on event floods (`:4827`), re-flooded on receipt (`:24568`) | topology beacon, raw unencrypted UDP | **Remove from the provisional path**, both directions. §7 already forbids a leaf originating pingwaves; the mirror rule — an anchor does not pingwave a provisional peer, and drops a pingwave *from* one — belongs in the same check. Counter, not silent drop. |
| 10 | **Heartbeat** to/from a provisional peer | liveness | **Constrain.** Permitted as E1 (§12 allows session maintenance), but it must stop the instant the provisional session is closed or expires, and it must not cause failure-detector *route* effects for an unadmitted peer. |
| 8 | **Grant drainer** running for a provisional peer's streams (`mesh.rs:27101`) | generic per-session machinery | **Constrain** to D1's two streams and the 64 KiB budget. It is required, but it is also caller-influenced state allocation, which §12 names explicitly. |
| — | **`route.rs` route installation from the routed handshake** (`mesh.rs:25460-25477` registers the peer with `registered_next_hop: source`) | `handle_routed_handshake` installs peer + routing state as part of accepting msg1 | **Constrain**: install the session, but hold routing/discovery participation until admitted. §12 already says this ("a provisional peer must not become a normal discovery/routing participant merely because the handshake installer populated a peer entry"); this is the concrete site. |
| 14 | fold / sensing / migration / route-withdrawal / `0x0D02` | emitted by a started node in the ordinary course | **Deny before effects.** None is reached by `Mesh::join`, so denying them costs the bootstrap nothing. |

## 4. Local-delivery check

**Confirmed: the envelope `connect_via` emits is addressed to the
anchor's own node id.**

- `Mesh::join` passes `rv.node_id` — the operator/anchor's node id from
  the rendezvous locator — as `peer_node_id`
  (`sdk/src/mesh_enroll.rs:290`; the locator is decoded at `:284`).
- The SDK forwards it unchanged: `sdk/src/mesh.rs:671-682`.
- The core stamps it as the envelope's destination:
  `let routing = RoutingHeader::new(dest_node_id, self.node_id as u32, DEFAULT_HANDSHAKE_TTL);`
  — **`src/adapter/net/mesh.rs:39562`** (`DEFAULT_HANDSHAKE_TTL = 16`,
  `:3867`), sent to `relay_addr`, which for `join` is the *same*
  anchor's address (`rv.addr`).

So on the anchor, `routing_header.dest_id == local_node_id` and
`mesh.rs:24614` takes the local-delivery branch. **The envelope is
local delivery, never transit** — exactly what §12 asserts, now cited.

**Every place on the anchor's ingress path that would forward such an
envelope instead of delivering it locally** — i.e. the §12 enforcement
points where "envelope to self is local delivery, not transit" must be
asserted, and where admission must be checked *before* the forward:

| # | site | what it forwards | current gate |
|---|---|---|---|
| F1 | `mesh.rs:24675-24757` — `dispatch_packet`'s `else` arm of the `dest_id == local_node_id` test; forward via `try_send_to` at `:24751` | any routed envelope whose `dest_id` is not us | only: not-in-protected-mode (`:24689`), TTL (`:24696`), route-table hit (`:24699`), partition filter (`:24703`). **No admission check.** |
| F2 | `mesh.rs:24602-24604` → `relay_protected_hop` `:24860`, forward at `:25057` | authenticated subnet route-hop envelopes | hop MAC authentication + gateway authority. **No admission check on the adjacent session.** |
| F3 | `router.rs:769` `Router::route_packet` → `enqueue` `:895` → drain `:984`/`:1028`/`:1034` | routed packets handed to the scheduler | TTL + route lookup + queue depth. **No admission check.** |
| F4 | `mesh.rs:24568` — pingwave re-flood inside `dispatch_packet` | pingwaves from any source | partition filter only. **No admission check.** |
| F5 | `mesh.rs:33389` / `:33443` — `forward_scoped_announcement` / `forward_capability_announcement` | capability announcements received from a peer | announcement verification. **No admission check.** |
| F6 | `mesh.rs:34104` `forward_punch_ack`, `:33706`/`:33726` rendezvous introduce | traversal messages on behalf of a third party | rendezvous budgets. **No admission check** (out of scope for a browser peer in v1, but the same rule applies: a provisional peer may not ask the anchor to introduce it). |
| F7 | `proxy.rs:349` `forward_and_send` | proxy-routed packets | proxy route table only. |

§12's "no third-party relay forwarding before enrollment, no
exceptions" therefore lands as: **F1–F7 each check admission of the
*adjacent* session before acting**, and F1 specifically must keep the
`dest_id == local_node_id` test *above* the admission check so that the
enrollment envelope (row 1) is delivered rather than refused.

## 5. Renewal

- The constant is **`RENEWAL_SERVICE = "net.mesh.renew"`**
  (`sdk/src/mesh_enroll.rs:42`) — *not* `net.mesh.enroll.renew`.
- `Mesh::renew` (`sdk/src/mesh_enroll.rs:235-267`) does the same two
  things `join` does: a **best-effort** `connect_via`
  (`:248-250` — its failure is deliberately ignored, because a device
  that just joined is already connected, `:243-247`) and then
  `call_typed(rv.node_id, RENEWAL_SERVICE, RenewalRequest, …)`
  (`:252-259`). The request is a `RenewalRequest::create(&device,
  current_chain)` (`:251`); the response is a `JoinOutcome` verified
  against the *same root* and bound to this device (`:263-266`).
- **Does renewal happen on an admitted session? Yes — and it should.**
  The device presents an existing `root → device` `DelegationChain`, so
  by construction it has already enrolled; the operator's handler
  (`serve_renewal_auto`, `:215-226`) refuses a revoked or lapsed grant
  (witnessed by `a_device_renews_its_grant_and_a_revoked_one_cannot`,
  `:441`, `:498-503`). Its `connect_via` is only there to re-establish a
  session after a restart.
- **Does anything in renewal need to be on the provisional allow-list?
  No — and it must not be.** The only case that looks like it needs one
  is a device renewing after a process restart: its session is gone, so
  the new handshake creates a *provisional* session, and the renewal
  call would be refused. That is the correct outcome and the right fix
  is not to widen the allow-list: such a device holds a valid grant and
  should be **admitted by presenting it**, either by re-running the
  bounded `net.mesh.enroll` exchange (which its existing chain
  satisfies) or by a Stage 4 admission check that accepts a valid
  unexpired chain at handshake time. Putting `net.mesh.renew` on the
  provisional list would give every unenrolled browser a second
  unauthenticated service surface for exactly one legitimate case.

## 6. What did not go cleanly

- **The reply-channel Subscribe is fire-and-forget UDP with a retry
  loop, and that shapes the allow-list's bounds more than anything
  else.** `send_membership_request_typed` (`mesh.rs:29634`) has no
  NACK and no retransmit window (`:29683-29686`), so it sends the same
  nonce up to `membership_max_attempts` times inside one
  `membership_ack_timeout` budget (`:29695-29703`). The allow-list
  therefore cannot say "one Subscribe frame" — it has to say "up to N
  frames sharing one nonce", and the anchor's dedupe
  (`recorded_membership_rejection`, `:29791`) is what makes that safe.
  A naive "exactly one frame" bound would break the existing flow on
  the first dropped datagram.
- **Row 5's frame is not a self-describing "enrollment message".** It
  is a generic `publish_to_peer` payload whose service name lives
  *inside* the nRPC envelope (`RpcRequestPayload.service`,
  `mesh_rpc.rs:5374-5380`), behind a channel-hash discriminator stamped
  on the wire. So §12 step 3 ("decode within strict bounds and validate
  the exact permitted action before its handler runs") means decoding
  the nRPC envelope *before* admission is decided — the check cannot be
  a cheap header test. That is a real cost and the plan should say so.
- **`0x0B00` is required-but-conditional and I could not make it
  unconditional either way.** The request stream is opened with the
  default byte-credit window (`open_stream_with`, `mesh.rs:35421`) and
  credit is charged per packet (`:35429-35433`); a single `JoinRequest`
  fits, so no grant is needed on the happy path. But a larger invite /
  tags payload, or a response above the window, makes a grant
  load-bearing. The allow-list has to permit `0x0B00` for the two
  bootstrap streams regardless, which is what row 8 and bound D say.
- **The heartbeat/pingwave pair is emitted from one loop body, two
  lines apart** (`mesh.rs:27554` and `:27556`). One is permitted
  session maintenance and the other is denied topology flooding, so the
  provisional check cannot be "skip the heartbeat loop for provisional
  peers" — it has to split those two statements. Easy to miss.
- **`Mesh::join` requires a started node, and starting is not a
  bounded act.** `start_arc` spawns seven loops (`mesh.rs:22389-22410`,
  `:22524`), three of which (heartbeat/pingwave, capability
  re-announce, direct-upgrade scan) produce traffic unrelated to
  enrollment. On the *device* side that is the device's own business;
  the finding is that "the bootstrap exchange" cannot be characterized
  by watching a joining node's socket — rows 10–13 will be on the wire
  regardless, and the allow-list is therefore about what the **anchor
  accepts**, never about what the device happens to send.
- **`connect_via`'s relay address and its destination node id are
  independent parameters** (`mesh.rs:39514`: `relay_addr: SocketAddr`,
  `dest_node_id: u64`). For `join` they name the same anchor, which is
  what makes §4's local-delivery claim true *for this flow* — but
  nothing in the signature enforces it. A provisional browser can call
  the same path with a third-party `dest_node_id`, and the anchor's F1
  branch would happily forward it. That is precisely the attack §12
  names, and it is one `if` away in the current code.
- **I did not find a distinct "method" concept to cite for row 5.**
  `net.mesh.enroll` is registered with `serve_rpc_typed(service, codec,
  handler)` (`sdk/src/mesh_enroll.rs:175`) — one handler per service
  name, no method dimension on the wire. So the allow-list's "method"
  is degenerate: `(service, unary)` is the whole identity. If Stage 4
  wants finer granularity it has to add it.
