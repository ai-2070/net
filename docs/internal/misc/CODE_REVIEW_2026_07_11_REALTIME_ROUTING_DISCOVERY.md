# Code Review — Real-Time Routing & Capability Discovery (RT-1..RT-5)

Branch `realtime-routing-discovery` vs `master`. Reviews the event-driven
capability-discovery + route-withdrawal work described in
`docs/plans/REALTIME_ROUTING_AND_DISCOVERY_PLAN.md` (slices RT-1..RT-5).

Method: extra-high-effort pass — ten independent finder angles over the diff,
then one adversarial verifier per surviving candidate against the working-tree
source. Every finding below carries its verifier verdict (CONFIRMED /
PLAUSIBLE) and quotes the decisive lines.

**Scope note — the branch moved during review.** Four follow-up commits plus an
uncommitted `WithdrawalSeqGate` change landed mid-pass and already fixed three
issues that would otherwise appear here:

- tool-registry duplicate rejection now atomic via `try_insert` — no change
  signal fired for a registration that never commits (`36beae5b1`).
- relayed sessions no longer promoted as **direct** routes on withdrawal
  (`c87ba6b27`) — but see Finding 1: the sibling graph-path promotion is still
  unguarded.
- pending trailing-edge flush now cancelled on rate-limit reset (`f44276a8e`) —
  but see Finding 12: the cancel is racy.

All line numbers refer to the working tree at review time (HEAD `c87ba6b27` +
uncommitted changes).

---

## Severity summary

| # | Severity | Area | Finding | Anchor |
|---|----------|------|---------|--------|
| 1 | High | RT-5 | Graph-path alternate promotion reinstalls the withdrawn route | `mesh.rs:8698` |
| 2 | High | RT-5 | Direct promotion resurrects a metric-1 route to a dead peer | `mesh.rs:8684` |
| 3 | High | RT-5 | Mixed-version degradation is documented wrong (old nodes don't drop 0x0C01) | `broadcast.rs:19` |
| 4 | High | RT-5 | O(E) `path_to` + inline AEAD on the receive loop; forged-origin DoS | `mesh.rs:8692` |
| 5 | High | RT-4/5 | False-positive failure poisons routes to a live node; recovery can't repair | `mesh.rs:3222` |
| 6 | Med-High | RT-5 | Withdrawal no-ops after a peer re-handshakes from a new address | `mesh.rs:8637` |
| 7 | Med-High | RT-3 | `start()` before `start_arc()` silently parks the announcer + keep-alive | `mesh.rs:4368` |
| 8 | Medium | RT-3 | RT-3 snapshot→write-back clobbers a concurrent explicit announce | `mesh.rs:4405` |
| 9 | Medium | RT-2/3 | Idempotent tool re-register fires a mesh-wide announce | `cortex/tool.rs:564` |
| 10 | Medium | RT-4 | Two unconditional flood rounds; 50 ms fragility; no trailing-edge coalesce | `mesh.rs:2167` |
| 11 | Low-Med | RT-1 | version/store inversion publishes a stale announcement | `mesh.rs:10850` |
| 12 | Low | RT-1 | Orphaned flush task steals a later deferral's claim | `mesh.rs:11021` |
| 13 | Low | RT-3 | `capability_announce_version()` over-counts on RPC-serving nodes (doc/test) | `mesh.rs:7595` |
| 14 | Cleanup | RT-1 | Deferred flush duplicates the announce fan-out loop | `mesh.rs:11029` |
| 15 | Cleanup | tests | New test files re-implement the shared `tests/common` harness | `route_withdraw.rs:26` |

Dropped after verification (not bugs): "destinations behind a failed peer aren't
withdrawn" (plan §4.3 explicitly scopes RT-5 poison-reverse to the sender's own
routes); "routed handshakes skip the event pingwave" (documented-intentional —
relayed sessions add no third-party forwarding capacity, and the DV rule-4 gate
would drop such a pingwave anyway); "damper empty-send burns the window"
(PLAUSIBLE but reachable only via the test-harness partition filter).

---

## Finding 1 — Graph-path alternate promotion reinstalls the withdrawn route (High)

**Verdict: CONFIRMED.** `mesh.rs:8696-8704` (`handle_route_withdrawal`).

The mid-review fix `c87ba6b27` guarded the **direct-session** promotion path
(now checks `ctx.addr_to_node.get(&addr) == Some(dest)`), but the
proximity-graph fallback directly below it was not given the same guard:

```rust
if let Some(path) = ctx.proximity_graph.path_to(&node_id_to_graph_id(dest)) {
    if let Some(first_hop) = path.get(1).map(graph_id_to_node_id) {
        if first_hop != from_node {
            if let Some(addr) = ctx.peer_addrs.get(&first_hop).map(|a| *a.value()) {
                let metric = (path.len() as u16).saturating_sub(2).saturating_add(2);
                ctx.router.routing_table().add_route_with_metric(dest, addr, metric);
                return;
            }
        }
    }
}
```

The only guard is `first_hop != from_node`. But `peer_addrs[first_hop]` can hold
the **relay's** address when `first_hop`'s session rides relay `from_node`
(`connect_via` installs `install_peer(dest, relay_addr, …)` at `mesh.rs:13444`;
the routed-handshake responder records `addr: source` — "NOT necessarily the
final responder's addr", `mesh.rs:5223-5228`). In that case
`peer_addrs[first_hop] == via_addr`, and since `remove_route_if_next_hop_is`
just emptied the slot, `add_route_with_metric` reinstalls exactly the
`(dest → via_addr)` route the withdrawal removed, then `return`s — also
suppressing the cascade. No liveness/failure-detector check either, so a Failed
`first_hop` with a retained `peer_addrs` entry is promoted too.

**Fix.** Apply the same directness test the direct path now uses (or resolve
`first_hop`'s real, verified address), and skip promotion when the resolved
next-hop address equals `via_addr` or the hop is not a live direct session.

---

## Finding 2 — Direct promotion resurrects a metric-1 route to a dead peer (High)

**Verdict: CONFIRMED.** `mesh.rs:8684-8690`.

The direct-session promotion consults `ctx.peers` / `ctx.addr_to_node` but never
the failure detector (which `DispatchCtx` carries at `mesh.rs:784`). Both maps
deliberately retain Failed peers — "the `peers` / `addr_to_node` / `peer_addrs`
maps are *not* evicted here. Keeping the session entry lets a transient-partition
recovery work" (`mesh.rs:3119-3129`) — with eviction only in the dead-peer sweep
at `session_timeout.saturating_mul(30)` (`mesh.rs:7009`).

Scenario (triangle A-B-C, all direct):

1. A dies. C's own detector marks A Failed; `ReroutePolicy::on_failure` repoints
   C's route to `(A via B)` (`reroute.rs:174`).
2. B's withdrawal for `dest=A` arrives. C drops `(A via B)`.
3. `peers.get(A)` is still `Some` and `addr_to_node[A_addr] == A`, so C calls
   `add_route(A, A_addr)` — a **metric-1** route (`RouteEntry::new`,
   `route.rs:430`) to the dead peer.
4. Pingwave installs use `hop_count + 2` (≥ 2) and replace only on strictly
   better metric (`route.rs:546`), so **no alternate can ever displace** the
   metric-1 dead route.
5. The `return` skips the cascade, so upstream nodes keep routing A through C.

Result: traffic to A black-holes until the stale-route sweep at
`session_timeout * 3` (`mesh.rs:6995,7064`) — ~90 s at the 30 s default — the
exact age-out latency RT-5 exists to eliminate. The `route_withdraw.rs` tests
only cover the chain case where C has no direct session to A, so this is
untested.

**Fix.** Gate the direct promotion on failure-detector liveness (don't promote a
peer the detector considers Failed/Suspected); fall through to graph/cascade
instead.

---

## Finding 3 — Mixed-version degradation is documented wrong (High)

**Verdict: CONFIRMED.** `broadcast.rs:18-20`, `mesh.rs:1269-1271`.

The docs claim nodes predating `SUBPROTOCOL_ROUTE_WITHDRAW` (0x0C01) "drop the
packets at subprotocol dispatch" and "degrade the same way by dropping the
unknown subprotocol." On `master` this is false. `process_local_packet` is a
chain of exact-id `if` checks (`SUBPROTOCOL_MIGRATION` … `SUBPROTOCOL_RENDEZVOUS`)
with **no catch-all / unknown-id guard**; after the last branch, control falls
into the standard event path, which:

- charges flow-control credit,
- inserts a `PendingStreamGrant` (so the old node sends a StreamWindow grant
  back), and
- pushes the 16-byte `RouteWithdrawal` payload onto the application inbound
  queue as a `StoredEvent` on stream id `0x0C01` (3073).

Subprotocol frames carry no segregating wire flag — `send_subprotocol` builds
with `PacketFlags::NONE` and `stream_id = subprotocol_id as u64` — so an old
binary genuinely surfaces undecodable binary events to application consumers on
every upgraded-peer failure/cascade.

(Config-disabled *upgraded* nodes are fine: `enable_route_withdraw` is checked
inside `handle_route_withdrawal`, after the 0x0C01 dispatch match returns — so
they drop cleanly. The problem is only genuinely old binaries.)

**Fix.** Either correct the docs to describe the real behavior, or — better — add
an unknown-subprotocol drop guard on the receive path so the intended "silently
ignore" degradation actually holds (a forward-compat guard future subprotocols
also benefit from).

---

## Finding 4 — O(E) `path_to` + inline AEAD on the receive loop; forged-origin DoS (High)

**Verdict: CONFIRMED (both legs).** `mesh.rs:8692` (`path_to`), `mesh.rs:8712`
(cascade flood), `spawn_route_withdrawal_flood` `mesh.rs:2262-2291`.

`dispatch_packet` runs on the **single** receive task (one `tokio::spawn` in
`spawn_receive_loop`, `mesh.rs:4646`; comment at `mesh.rs:4908` confirms
"`dispatch_packet` runs on the single receive task"). `handle_route_withdrawal`
does all of this synchronously before the next packet is processed:

- `ProximityGraph::path_to` rebuilds the full adjacency map from every edge, then
  BFS — O(E) per call (`proximity.rs:858-893`). This is a **new** O(E) cost on
  the receive path; its only other caller is `reroute.rs`, reached from the
  failure-detector callback (a different task).
- `spawn_route_withdrawal_flood` builds + AEAD-encrypts one packet **per peer
  inline** (`get_or_create_stream` + `build_subprotocol`), and only the socket
  sends are offloaded to a spawned task. Contrast `forward_capability_announcement`
  (`mesh.rs:9067`) and pingwave re-broadcast (`mesh.rs:4833`), which spawn the
  whole build+encrypt+send loop — the withdrawal flood is the outlier.

**Amplification / DoS.** The pingwave source gate only checks `addr_to_node` for
the *sender*; the pingwave **origin is unauthenticated wire data** (`pw.origin_id`,
`mesh.rs:4769`), deduped only on `(origin, seq)`. A registered direct peer can
forge pingwaves for thousands of distinct fake origins, each installing a route +
graph edge, then batch-withdraw them. Every withdrawal passes the seq gate,
finds no alternate, runs the full O(E) `path_to`, and cascades an O(peers)
inline-crypto flood. The damper is **per-dest only** (`mesh.rs:2233`), so distinct
fake dests never share a damping window: one attacker packet amplifies to
O(peers) packets + O(E) work, and N dests give O(N·E) pinned on the victim's
dispatch loop.

**Fix.** Move `path_to` and the per-peer packet build off the receive task
(spawn the whole cascade like the sibling flood paths do); consider an aggregate
rate cap on cascades (not just per-dest) and/or authenticated pingwave origins.

---

## Finding 5 — False-positive failure poisons routes to a live node; recovery can't repair (High)

**Verdict: CONFIRMED (both legs, topology-dependent).** `mesh.rs:3222`
(emission), `mesh.rs:3241` (recovery).

**(A) The withdrawal is flooded even when the emitter rerouted successfully.**
The `on_failure` closure runs `rp_failure.on_failure(node_id)` first (which
installs an alternate route to the failed node, `reroute.rs:174`), then floods
`RouteWithdrawal{dest: node_id}` unconditionally (gated only on the config flag +
damper, `mesh.rs:3222-3232`). So a node that can *still forward* toward `dest`
via its alternate nonetheless tells the mesh "dest unreachable via me."

**(B) The recovery pingwave cannot undo the damage.** On a false positive (a
transient stall, at tight detector timeouts), receivers that had only
`(dest via emitter)` drop it and, lacking an alternate, cascade their own
withdrawal — losing reachability to a **live** node. The `on_recovery` closure
emits an `origin = self` pingwave; but `EnhancedPingwave` is a fixed 72-byte
struct with no edge list (`proximity.rs:23-27`), and receivers install routes
**to the pingwave's origin only** (`add_route_with_metric(origin_nid, source, …)`,
`mesh.rs:4807`). So B's recovery pingwave installs routes to B, never restoring
third parties' routes to A. The closure's own comment concedes it only
"refreshes routes THROUGH us." Repair waits for A's own next heartbeat pingwave
to propagate — up to `heartbeat_interval` per hop.

This is a **regression vs master**, which has no withdrawal mechanism: pre-RT-5 a
transient false positive touched only the emitter's local table and third
parties kept `(A via B)`. Scope caveat: harm requires the receiver to have no
alternate (sparse/chain topologies); dense meshes self-heal via the receiver-side
promotion.

**Fix.** Don't emit the withdrawal when `on_failure` installed a viable
alternate (the node *can* still forward). More generally, reconsider whether a
single-observer failure verdict should poison mesh-wide routing before the loss
is corroborated.

---

## Finding 6 — Withdrawal no-ops after a peer re-handshakes from a new address (Med-High)

**Verdict: CONFIRMED.** `mesh.rs:8637`, `route.rs:578`, `route.rs:546`.

`handle_route_withdrawal` resolves `via_addr` from the peer's **current**
`PeerInfo.addr` and calls `remove_route_if_next_hop_is(dest, via_addr)`, which is
strict `SocketAddr` equality (`route.rs:578`). But multi-hop routes were
installed with the peer's *then*-current address, and `add_route_with_metric`
never rewrites the stored next-hop on equal-metric refresh — it only bumps
`updated_at` (`route.rs:546-557`, locked in by test
`add_route_with_metric_equal_does_not_overwrite_next_hop`). No re-handshake path
migrates existing routing-table entries to the new address.

So after a peer re-handshakes from `addr2` (NAT rebind; accepted only once the
old session is silent past `session_timeout`), equal-metric pingwaves from
`addr2` keep the stale-`addr1` route entries alive indefinitely. Every subsequent
withdrawal from that peer resolves `via_addr = addr2`, mismatches the `addr1`
entry, and the handler returns before promotion or cascade — RT-5 silently
degrades to age-out for that peer.

**Fix.** On re-handshake, migrate routing-table entries whose next-hop was the
peer's old address to the new one (or key routes by peer node-id rather than raw
`SocketAddr` for the withdrawal match).

---

## Finding 7 — `start()` before `start_arc()` silently parks the announcer + keep-alive (Med-High)

**Verdict: CONFIRMED.** `mesh.rs:4368` (RT-3 loop), `mesh.rs:4106` (`start_arc`),
`mesh.rs:3975` (`start` idempotency).

`spawn_capability_announce_on_change_loop` captures `self.self_weak.get().cloned()`
once at spawn and parks silently on `shutdown_notify` when it is `None`:

```rust
let (Some(weak), false) = (weak, debounce == Duration::MAX) else {
    let _ = shutdown_notify.notified().await;  // no log, no retry
    return;
};
```

`start()` is idempotent (`if self.started.swap(true, …) { return }`), and
`start_arc()` sets `self_weak` then calls `start()`. So `node.start(); …;
node.start_arc();` spawns the RT-3 loop (and the pre-existing keep-alive loop,
`mesh.rs:4314`) with `weak = None`, and the later `start_arc` can't respawn them
— both park forever with zero diagnostics. Capability changes then propagate only
via explicit `announce_capabilities` calls; later-registered tools stay invisible
mesh-wide.

This ordering is present in the repo's own tests: `capability_broadcast.rs`'s
`handshake` helper calls `start()`, and the RT-1 tests then call `start_arc()`
under the comment "Idempotent." Those tests pass only because the RT-1 flush path
re-reads `self_weak` per call (`mesh.rs:10945`), whereas the RT-3/keep-alive loops
capture it at spawn. `integration_tool_announce.rs:613` documents the trap in a
comment — but the knowledge lives only in a comment, not in code. This is
new-in-branch exposure (the RT-3 loop) layered on a pre-existing keep-alive
footgun.

**Fix.** Make `start_arc()` respawn (or refuse to no-op) the `self_weak`-dependent
loops if `start()` already ran; at minimum, have the parked branch `tracing::warn!`
so the misconfiguration is visible. Better: have both loops re-read `self_weak`
per iteration like the flush path does.

---

## Finding 8 — RT-3 snapshot→write-back clobbers a concurrent explicit announce (Medium)

**Verdict: CONFIRMED (mechanism exact; window narrow).** `mesh.rs:4405-4406`,
write-back at `mesh.rs:10723`.

The RT-3 loop does `let caps = node.user_caps_snapshot();` then
`node.announce_capabilities_with(caps, ttl, true).await`, and
`announce_capabilities_with` writes its argument back:
`*self.user_caps.write() = Some(caps.clone())`. No lock spans the pair. If an
explicit `announce_capabilities(Y)` interleaves between the snapshot and the
write-back:

1. RT-3 snapshots `user_caps = X`.
2. App announces `Y` (writes `user_caps = Y`, version `v`, broadcasts).
3. RT-3's announce rewrites `user_caps = X`, broadcasts at version `v+1`.

Receivers honor the highest version per node, so `Y`'s tags vanish mesh-wide, and
all later announces (keep-alive, RT-3, chain-tag) re-derive from the clobbered
`X` — the tags stay lost until the app manually re-announces. The snapshot→
write-back gap is sync code (no `.await` between), so it needs true thread
parallelism, but RT-3 makes concurrent announces routine rather than 150 s-rare.
The identical pattern pre-exists on master's keep-alive loop; the diff's
contribution is frequency.

**Fix.** Serialize the read-modify-write of `user_caps` under one lock across the
snapshot and the announce, or have the change-driven announcer merge rather than
overwrite the baseline.

---

## Finding 9 — Idempotent tool re-register fires a mesh-wide announce (Medium)

**Verdict: CONFIRMED.** `cortex/tool.rs:564` (`insert` → `signal_changed`).

`ToolMetadataRegistry::insert` calls `signal_changed()` unconditionally, even
when the new descriptor is byte-identical to the previous one. The comment claims
"the consumer diffs," but the RT-3 consumer does not — it broadcasts a full
capability announcement (bumped version) plus the piggybacked pingwave flood on
every signal. So an app using an ensure-registered pattern (periodically
re-inserting the same `ToolDescriptor`) generates a mesh-wide announcement +
pingwave flood every time, forever, with zero information change.
`LocalServiceRegistry::insert` already suppresses idempotent re-serves, so the
two registries disagree on this contract.

**Fix.** Skip `signal_changed()` when the new descriptor equals the prior entry
(`insert` already has `prev` in hand), matching `LocalServiceRegistry`.

---

## Finding 10 — Two unconditional flood rounds; 50 ms fragility; no trailing-edge coalesce (Medium)

**Verdict: CONFIRMED (A/B/C).** `spawn_event_pingwave` `mesh.rs:2167`;
`EVENT_PINGWAVE_RESEND_DELAY` `mesh.rs:2196`; gate `mesh.rs:2143`.

- **(A)** The 50 ms resend delay is a heuristic with no feedback. Round 2 exists
  to outlast the initiator's post-handshake `addr_to_node` install (which its own
  comment admits lands "in microseconds in practice"). If the initiator's task
  stalls > 50 ms, both rounds hit the rule-4 unregistered-source drop
  (`mesh.rs:4797`), the min-gap gate absorbs further events, and that edge
  converges only at the heartbeat tick (bounded ~5 s at defaults). The deeper fix
  — install `addr_to_node` before sending the final handshake message, or let the
  responder emit after the initiator's first post-handshake packet — was not
  taken. (Loss is asymmetric: the responder installs before emitting, so the
  initiator's own flood still lands.)
- **(B)** `for round in 0..2u8` runs for **all four** emission sites; the race it
  addresses exists only for session-open. Recovery (`mesh.rs:3241`) and
  change-announce-piggyback (`mesh.rs:4416`) pay a full duplicate mesh-wide flood
  (fresh seq each round, so nothing dedups) for no benefit. A per-source
  `resend: bool` would halve the cost for two of the four sites.
- **(C)** The gate is leading-edge-only with a plain early return — events inside
  `event_pingwave_min_gap` (250 ms default) are silently absorbed with no
  trailing-edge catch-up, the same silent-drop pattern RT-1 was built to fix for
  announces. Softened by per-edge symmetry (the counterparty emits through its own
  gate), so third parties usually still learn each edge from the other end.

**Fix.** Gate round 2 behind a per-source flag (session-open only); add a
trailing-edge coalesce to the min-gap gate; consider fixing the handshake
ordering so round 2 isn't needed at all.

---

## Finding 11 — version/store inversion publishes a stale announcement (Low-Med)

**Verdict: CONFIRMED (self-healing).** `mesh.rs:10850` (`fetch_add`),
`mesh.rs:10912` (`store`).

`capability_version.fetch_add` and `local_announcement.store` are ~60 lines apart
with no spanning lock. Two concurrent announcers can take v5/v6 and store
6-then-5, leaving `local_announcement` at v5. The RT-1 flush and every
session-open `push_local_announcement` then rebroadcast v5, which peers that saw
v6 reject as stale (fold merge rejects `generation ≤ stored`,
`fold/mod.rs:142`). The window is sync-only (Ed25519 sign + fold lock), so it
needs a multi-threaded runtime; it self-heals at the next announce/keep-alive.
Pre-exists on master; RT-3 widens exposure.

**Fix.** Hold one lock across the version bump and the `local_announcement`
store, or stamp the version into the stored announcement atomically.

---

## Finding 12 — Orphaned flush task steals a later deferral's claim (Low)

**Verdict: CONFIRMED (no content loss).** `flush_deferred_announce`
`mesh.rs:11021`; `spawn_deferred_announce` `mesh.rs:10992`.

Deferred flush tasks are identified only by the shared `deferred_scheduled` bool
— no generation/token ties a woken task to the deferral that claimed the slot.
The `f44276a8e` reset now clears the flag but cannot cancel the already-parked
tokio task. Timeline (`min_announce_interval = 10 s`):

- t=0 v1 broadcasts; t=1 v2 defers → task T1 parked for t=10.
- t=5 `set_reflex_override` clears the flag (T1 orphaned but still parked).
- t=6 v3 broadcasts unconditionally (`last_broadcast_at = None`), opening window
  [6,16).
- t=7 v4 re-claims the flag, parks T2 for t=16.
- t=10 T1 wakes, passes `if !deferred_scheduled` on **T2's** claim, broadcasts
  4 s into the fresh window and shifts window end to t=20; T2 no-ops at t=16.

Contradicts the reset's own comment ("would put a second broadcast inside the
fresh window"). No content is lost (T1 broadcasts the latest
`local_announcement`); harm is an extra in-window broadcast plus window drift.

**Fix.** Tag each deferral with a generation counter; the flush task compares its
captured generation against the current one and no-ops on mismatch.

---

## Finding 13 — `capability_announce_version()` over-counts on RPC-serving nodes (Low, doc/test)

**Verdict: CONFIRMED (doc/test-contract, not runtime).** `mesh.rs:7595` (doc),
`mesh.rs:10677` (self-index bump).

The getter's doc says "the delta counts announce *calls*," but
`index_self_with_local_services` — called by `serve_rpc` (`mesh_rpc.rs:2109`),
explicitly "Sync (no broadcast)" — bumps the same `capability_version`. One
`serve_rpc` moves the counter by 2–3 for one broadcast. Runtime is unaffected
(the counter's only runtime role is a monotonic announcement version where
over-bumping is harmless, as the sibling comment at `mesh.rs:10652` notes). The
RT-3 burst-count test (`registry_burst_coalesces_into_one_announce`, asserts
`version_before + 1`) is valid only because it never serves RPC.

**Fix.** Correct the getter's doc (state that `serve_rpc`'s self-index also bumps
it); optionally note the assumption in the RT-3 test.

---

## Finding 14 — Deferred flush duplicates the announce fan-out loop (Cleanup)

`flush_deferred_announce` (`mesh.rs:11029`) is a verbatim copy of the broadcast
loop at the tail of `announce_capabilities_with` — same `peers` snapshot, same
`send_subprotocol(addr, SUBPROTOCOL_CAPABILITY_ANN, &bytes)`, same
trace-and-continue. When the primary path later gains partition filtering, a
signing step, concurrent sends, or per-peer metrics, the RT-1 flush silently
keeps the old behavior, so rate-limited announces take a different, untested send
path than immediate ones — and the divergence only surfaces inside a rate-limit
window. Extract one `broadcast_announcement_bytes(&self, bytes)` helper both call.

---

## Finding 15 — New test files re-implement the shared `tests/common` harness (Cleanup)

`route_withdraw.rs` and `event_pingwave.rs` each carry private copies of
`build`/`handshake`/`wait_until` (and `PSK`/buffer constants) that
`tests/common/mod.rs` already centralizes as `build_node_with` / `connect_pair` /
`await_condition`, including duplicating the load-bearing "initiator must be the
already-started node" gotcha as a cross-file comment. There are now 3–4
near-identical harness copies with drifting poll cadences (25 ms here vs the
shared helper). A CI-flake fix or a `start_arc`-ordering fix applied to
`common/mod.rs` never reaches these files, and they lose the harness's
invariant-naming diagnostics. Use `mod common; use common::*;` as
`failure_detector_matrix.rs` does.

---

## Appendix — verification method

Each candidate was checked by an independent verifier agent given the diff, the
working-tree source, and the specific claim, returning CONFIRMED (named
inputs/state + wrong output, decisive line quoted) / PLAUSIBLE (mechanism real,
trigger uncertain) / REFUTED (guarded elsewhere, quoted). Only CONFIRMED and
PLAUSIBLE findings are recorded above. The three dropped candidates listed in the
severity summary were REFUTED or classified as accepted-scope per the plan doc.
