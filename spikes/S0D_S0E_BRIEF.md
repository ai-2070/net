# Slice 4 — Stage 0 / S0d + S0e: send/ingress inventory and bootstrap frame inventory

Source of truth: `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`
at HEAD. Read §1 ("The send path is part of the refactor…"), §2
("Inbound", "Outbound", "What S0b established"), §5 Layer 0, §12 (the
browser admission contract), Stage 0 S0d/S0e, Stage 1, Stage 4. Only
Stage 0 is authorized. These are the last two Stage 0 outputs. Both are
**desk inventories — no code, no spike crate**; the deliverables are two
reports whose every row cites `file:line` at the commit you read.

## S0c review verdict (for context)

Accepted at `80fbe57bf`. The threshold-1 analysis was the right call: my
threshold measured A's absolute cost, not A−B, and you reported both
honestly. Plan updated (§3 `MAX_PAYLOAD_SIZE` black hole + batch-per-packet,
§4 numbers and "stays deferred", Stage 3 `validate()` counter, Stage 7).
No follow-ups owed from S0c.

## S0d — send/ingress inventory and proposed seam contract

### Target

`net/crates/net/src/adapter/net/` — `mesh.rs`, `mod.rs`, `proxy.rs`,
`router.rs`, `transport.rs`, `reroute.rs`, `failure.rs`, `traversal/**`,
`behavior/**` (only where they touch a socket). Read-only.

### Change

1. **Outbound inventory.** Enumerate every site where a packet leaves the
   process: every `socket.send_to(` / `.send(` on a `NetSocket` /
   `PacketSender` / raw `UdpSocket`, every `bound_datagram_send(`, every
   scheduler `enqueue(` that ends in a socket write, and the scheduler
   drain itself (`router.rs` `sendmmsg` grouping). §1's list of ~30
   `mesh.rs` sites is the starting point; re-derive it at HEAD — the file
   moves. For **each** site, one table row:
   - `file:line`, enclosing fn;
   - **class**: stream data / routed data / forwarded-for-others /
     retransmission / heartbeat / pingwave / subprotocol reply (which id) /
     handshake msg / traversal (reflex, rendezvous, keep-alive) / publish /
     transfer control / other;
   - **destination typing today**: `SocketAddr` from `PeerTransport::send_addr()`
     / from `RouteEntry::next_hop` / from the packet source / literal
     config / other — say which;
   - **blocking semantics**: awaited, `bound_datagram_send` + which
     deadline, fire-and-forget `let _ =`, spawned task, scheduler-queued;
   - **error mapping**: what the caller does with `Err` (mapped to
     `StreamError::Transport`, `AdapterError::Connection`, logged, ignored,
     counted — name the counter);
   - **batching**: per-packet vs `sendmmsg` group vs `Batch`;
   - **guard hazard**: whether a `DashMap` guard or other lock is held
     across the await (the repo has had this bug — cite the
     `org_routing_wiring_tests.rs:7786` note);
   - **RTC disposition you propose**: for a `PeerAddr::Rtc` destination,
     which of §2's dispositions applies — refuse-at-admission / drop with
     counter / defer / not applicable (UDP-only by nature, e.g. traversal).
2. **Ingress inventory.** Every path by which bytes reach
   `dispatch_packet` (`mesh.rs`, `fn dispatch_packet`) or bypass it: the
   `IngressReceiver` variants, the batched-ingress path, any secondary
   receive loops (`mod.rs` single-peer, `proxy.rs`, `router.rs`, traversal
   sockets), and anything that injects synthetic packets. For each: entry
   point, source typing (`SocketAddr`), ordering guarantees, what happens
   on `ConnectionReset`, whether it runs on the single dispatch owner.
3. **Proposed seam contract**, as a short design section, not code:
   - the UDP-preserving submission API (name, signature, which of the
     rows above call it, and a statement that every row's blocking /
     deadline / error-mapping / batching column is unchanged after the
     edit — that is Stage 1's exit criterion);
   - the RTC submission API (§2's `try_send`, bounded, non-blocking, with
     reserved bytes/slots — restate it against the rows it will serve);
   - the scheduler drain split (UDP `sendmmsg` group vs RTC per-peer
     queue);
   - the bounded RTC input on `IngressReceiver` (§2 "one dispatch
     owner"), including what `ConnectionReset` on the RTC socket does
     (S0b 7.4) and how per-source ordering is preserved across UDP and
     RTC inputs;
   - the list of rows where the destination is *derived from a packet
     source* or *from a route entry* rather than from `PeerTransport` —
     those are where `PeerAddr` will leak into `route.rs` / `reroute.rs`
     and Stage 1 must know the count.
4. **Counts.** Total outbound sites by class and by blocking semantics;
   total ingress entries; number of sites holding a guard across an
   await (expect zero — if not zero, that is a finding).

### Report

`docs/internal/spikes/S0D_SEND_INGRESS_INVENTORY.md`: commit hash; the
outbound table; the ingress table; the proposed contract; the counts;
"did not go cleanly" (anything ambiguous, anything §1 got wrong, any site
that resists classification).

## S0e — bootstrap frame inventory

### Target

`net/crates/net/sdk/src/mesh_enroll.rs`, `sdk/src/enrollment.rs`,
`sdk/src/mesh.rs` (`connect_via`, `call`), and the core paths they hit in
`adapter/net/mesh.rs` (`connect_via`, the responder handshake path, nRPC
dispatch, channel membership `0x0A00`, stream window `0x0B00`,
reliability). Read-only.

### Change

Trace `Mesh::join` from the invite string to the verified grant and list
**every frame** the exchange puts on the wire or requires the anchor to
accept, in order, as one table:

- step; direction (device→anchor / anchor→device);
- outer format (Net header / routing envelope / route-hop / keep-alive);
- subprotocol id and the **specific action** inside it (e.g. `0x0A00`
  *join channel X*, not "0x0A00"); for nRPC: service name, method,
  unary/streaming, reply channel used;
- stream id / reliability flag / whether a window grant (`0x0B00`) is
  required for it to complete;
- `file:line` where it is sent and where it is handled;
- whether it is **required** for enrollment or **incidental** (something
  the current flow does that a bounded bootstrap exchange would not need
  — e.g. an announcement, a heartbeat, a pingwave, a capability query, a
  subscription wider than the reply channel). §12 says incidental work is
  removed or constrained, never allowed — so this column is the output.

Then, separately:

1. **The action-level allow-list** §12 needs, derived from the required
   rows only: the exact set of (outer format, subprotocol, action,
   bounds) a provisional session may exercise. State bounds (max frames,
   max bytes, max streams, max channel memberships, deadline).
2. **Incidental work found**, with the recommended handling for each
   (remove from the join flow / constrain to the bootstrap bound / defer
   until admitted).
3. **Local-delivery check**: confirm the routed envelope `connect_via`
   emits is addressed to the anchor's own node id (cite the `dest_id`
   assignment), and list every place on the anchor's ingress path that
   would *forward* such an envelope rather than deliver it locally — those
   are the §12 enforcement points where "envelope to self is local
   delivery, not transit" must be asserted before admission is checked.
4. **Renewal**: `RenewalRequest` also rides `connect_via` +
   `net.mesh.enroll.renew` (or whatever the constant is — cite it). State
   whether renewal happens on an *admitted* session (it should) and
   whether anything in it needs to be on the provisional allow-list (it
   should not).

### Report

`docs/internal/spikes/S0E_BOOTSTRAP_FRAMES.md`: commit hash; the frame
table; the allow-list; incidental work; local-delivery check; renewal;
"did not go cleanly".

## Constraints (both)

- No code changes anywhere. Only `docs/internal/spikes/**` and, if you
  need scratch scripts for counting, `spikes/tools/**`. Do not touch
  `net/**`, `go/**`, `web/**`, `.github/**`, or any plan document.
- Every row cites `file:line` at the commit you read; state that commit
  at the top of each report. Use the repo's CodeGraph (`codegraph
  explore`, `codegraph callers`) where it saves reading — `AGENTS.md`
  describes it — but the citations must be verified against the file.
- Skip formatters, linters, the project-wide suite; nothing in
  `net/crates/net` needs to be built.
- One commit on `LZL0/webrtc-transport`, prefix `spike(s0d,s0e):`.
  `git status` clean after.
- Reply in the terminal with: the commit hash; S0d's counts (outbound
  sites by class, ingress entries, guard-across-await count); S0e's
  allow-list verbatim and the incidental-work list; both "did not go
  cleanly" lists verbatim.

## Acceptance

- Both reports exist with every section named above.
- S0d's outbound table has at least as many `mesh.rs` rows as §1 lists
  (~30) plus the `mod.rs`, `proxy.rs`, `router.rs` rows, each with all
  eight columns filled — no "TBD".
- S0e's frame table distinguishes required from incidental for every
  row, and the allow-list is derived only from required rows.
- `git diff --stat HEAD~1` touches only `docs/internal/spikes/` (and
  `spikes/tools/` if used).
