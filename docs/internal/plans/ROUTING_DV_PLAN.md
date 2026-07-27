# Pingwave-driven distance-vector routing — fill in the gaps

## Status

**Shipped.** All plan steps are implemented and under test:

- `ProximityGraph::edges` populated on every pingwave receipt (`insert_or_update_edge`) and aged out in lockstep with `RoutingTable::sweep_stale` via `sweep_stale_edges` (atomic `DashMap::retain` — no collect-then-remove race).
- Four loop-avoidance rules at the pingwave-dispatch boundary in `mesh.rs`:
  1. Origin self-check — drop if `origin == self_id`.
  2. `MAX_HOPS = 16` — drop if `hop_count >= MAX_HOPS`.
  3. Split horizon on re-broadcast — skip peers whose address equals the next-hop for the origin.
  4. **Unregistered-source rejection** — drop pingwaves from peers that haven't completed a handshake; only `addr_to_node.get(&source).is_some()` may inject routing state. Added post-review to close a route/topology poisoning vector.
- Latency EWMA (`α = 1/8`) on `ProximityEdge.latency_us` for equal-hop tie-breaking (populated; live tiebreaking at lookup is deferred until the routing table grows to hold multiple routes per destination).
- `RoutingTable::lookup_alternate(dest, exclude_next_hop)` returns the installed entry if it isn't excluded; scaffolding for a future multi-route-per-destination table.
- `ReroutePolicy::on_failure` resolution order: `lookup_alternate` → `find_graph_alternate_for` → any direct peer.

Unit + integration tests in place; before/after regression (`test_regression_dv_path_to_returns_multi_hop`) confirms `path_to` now returns real multi-hop paths where it used to always return `None`. See `TRANSPORT.md` § Routing for the caller-facing summary.

---

*Original design notes preserved below for context.*

The seed of pingwave-driven route installation was already in `mesh.rs` dispatch: when node X receives a pingwave originated by Y, forwarded via direct peer Z, X calls `RoutingTable::add_route_with_metric(Y, next_hop=Z, metric=pw.hop_count + 2)`. The metric policy in `add_route_with_metric` preserves the lower-metric entry, so direct routes (metric 1) always beat pingwave-installed routes. Routes age out via `sweep_stale` on the heartbeat-loop tick.

This plan filled the remaining gaps — **topology graph population**, **cheap loop avoidance**, and **metric polish** — without inventing a full DV stack. The pingwave wire format didn't change. Most of this was policy and plumbing on top of machinery that was already in place.

## What already works

- **Pingwave emission** (`mesh.rs:spawn_heartbeat_loop`) — every `heartbeat_interval`, each node emits one `EnhancedPingwave` (72 bytes, unencrypted) to every connected peer.
- **Pingwave forwarding** (`mesh.rs:743-757`) — receivers re-broadcast to all peers except the sender; TTL decremented, `hop_count` saturating-incremented.
- **Route install on receipt** (`mesh.rs:735-741`) — `(origin, next_hop=source, metric=hop_count+2)` goes into the routing table; metric policy keeps the best.
- **Age-out** — `RoutingTable::sweep_stale` removes entries older than `max_route_age` (configured as `3 × session_timeout`). Called from the heartbeat loop.
- **Metric policy** — `add_route_with_metric` preserves a strictly-better existing metric; equal-or-better replaces. Direct route (`add_route` → metric 1) always wins.
- **Reactive reroute** — `ReroutePolicy::find_graph_alternate_for` queries `ProximityGraph::path_to()` on failure to pick an alternate per-destination.

## Gaps this plan fills

1. **`ProximityGraph::edges` is never populated.** The struct holds an `edges: DashMap<NodeId, Vec<NodeId>>` that `path_to()` BFS-walks, but nothing ever inserts into it. `path_to()` therefore returns `None` for every multi-hop destination today, and `ReroutePolicy::find_graph_alternate_for` falls through to the "any direct peer" fallback. The routing-table installer works; the graph-based lookup does not.

2. **Missing cheap loop-avoidance rules.** Pingwave re-broadcast fans out to every peer except the sender. That's TTL-bounded but not split-horizon-bounded, and there's no defensive cap on runaway `hop_count` or on pingwaves that claim `origin_id == self_id` (which a misbehaving peer — or a stale buffered packet from ourselves — could inject).

3. **Metric is hop count only.** Pingwaves already carry `origin_timestamp_us`; we never use it to break ties between equal-hop paths, so a 1-hop 200 ms satellite link is indistinguishable from a 1-hop 5 ms fiber link.

4. **No explicit "table is authoritative, graph is input" boundary.** `ReroutePolicy::on_failure` goes straight to `path_to()` without first checking whether the routing table already has a viable alternate. Route lookups should always hit the table first; the graph is a topology derivation used for (a) populating the table via pingwave receipt and (b) synthesizing one-off alternates when the table has nothing.

**Explicitly not a gap — route invalidation.** The existing `sweep_stale` age-out is enough for v1: if the `Z → Y` path breaks but Z itself stays up, Z stops receiving fresh pingwaves from Y, stops re-broadcasting them, and X's `(Y, via=Z)` route ages out on its own on the heartbeat sweep. No extra invalidation messaging or per-source `last_refreshed` tagging is needed.

## Goals

- **Populate `ProximityGraph::edges`** from pingwave hops so `path_to()` returns real multi-hop paths.
- **Cheap loop avoidance:** split-horizon on re-broadcast (don't re-advertise a destination on the same link we'd use to reach it), a `MAX_HOPS` cap, and a pingwave origin self-check (drop and never install routes for `origin_id == self_id`).
- **Latency tie-break on the metric:** hop count stays the primary metric; a smoothed one-way delay estimate (`now_us − origin_timestamp_us`, EWMA'd per origin) breaks ties between equal-hop paths.
- **`RoutingTable` is authoritative; `ProximityGraph` is an input and a fallback.** `send_routed` already only consults the table. Reroute should too — falling back to `path_to()` only when the table has no alternate.

## Non-goals

- **Not OSPF / BGP.** No link-state flooding, no sequence-number-per-link routing DB, no path-vector attribute lists. DV + poison reverse is sufficient for the mesh sizes Net targets.
- **No wire-format changes.** Pingwave stays 72 bytes. Destination advertisements are *implicit* in pingwave propagation — the origin advertises itself; intermediate hops relay-install routes to the origin as a side effect. This is the existing shape, just made safer.
- **Not a secure routing protocol.** Pingwaves are unencrypted and unsigned today; a malicious peer can forge origin ids and inflate/deflate hop counts. That's a separate protocol concern (`PINGWAVE_AUTH_PLAN.md` if we ever need it). This plan assumes mutually-trusted participants or a PSK-gated mesh.
- **No geographic / latency-tier routing.** Metric is `hop_count` (+ optional latency estimate), not a multi-dimensional policy. Teams that need "prefer WiFi over cellular" build that on top.

## Design

### 1. Populate edges on pingwave receipt

When X receives a pingwave for origin Y via direct peer Z, X knows one concrete topology fact: **Z has a route to Y** (otherwise Z wouldn't be forwarding the pingwave). Insert the directed edge `Z → Y` into `ProximityGraph::edges` on every pingwave receipt. X itself always has the edge `X → Z` (Z is a direct peer), inserted at session setup. Combined, X's local graph has enough to BFS a 2-hop path `X → Z → Y`; deeper chains accumulate as pingwaves traverse.

Edges are timestamped on insert and aged out at the same cadence as `RoutingTable::sweep_stale` (which already runs on every heartbeat-loop tick).

### 2. Three cheap loop-avoidance rules

No full DV protocol — just three rules at the pingwave receive / re-broadcast boundary:

**2a. Origin self-check.** If `pw.origin_id == self_id`, drop the pingwave. Do not install routes. This handles a malicious or buggy peer that echoes our own origin back at us, and also a stale buffered pingwave we emitted earlier that a partitioned-then-healed peer just replayed.

**2b. `MAX_HOPS` cap.** If `pw.hop_count >= MAX_HOPS` (propose 16), drop the pingwave on receipt. The existing TTL field already provides a forwarding-time cap; this is the receive-time counterpart so an inflated-hop-count advertisement can't install a usable route. Metric-policy prevents replacement of a shorter route, but `MAX_HOPS` also avoids populating the routing table and edges map with arbitrarily distant entries.

**2c. Split horizon on re-broadcast.** Before emitting a pingwave forward to peer P, consult `RoutingTable::lookup(origin_nid)` — if the next-hop for `origin_nid` equals P's address, skip P. This is lightweight poison-reverse: we never advertise a route for Y to the peer we'd use to reach Y. Prevents P from learning "X can reach Y in N+1 hops" and installing a backward loop.

These three rules are enough for the convergent topologies Net targets. Count-to-infinity is bounded by `MAX_HOPS + age-out`, not by a dedicated protocol.

### 3. Metric: hops primary, latency tie-break

Keep `metric = hop_count + 2` as the primary ordering (existing code; the `+2` keeps direct routes at metric 1 strictly better than any pingwave-installed route). Add a **secondary latency tie-break** for equal-hop paths:

- On each pingwave receipt for origin Y via peer Z, compute `sample_us = now_us.saturating_sub(pw.origin_timestamp_us)`.
- Maintain an EWMA per `(origin_nid, next_hop_addr)` pair: `latency_ewma_us = α·sample + (1−α)·latency_ewma_us`, with `α = 1/8`.
- When comparing two equal-hop candidates, prefer the lower EWMA.

`ProximityGraph` already owns per-node state; extend `ProximityNode` with `latency_ewma_us: AtomicU32` keyed per origin. When `RoutingTable::lookup` returns multiple equal-metric entries (future-proofing — today it returns one), the tie-breaker applies.

Clock skew caveat: `origin_timestamp_us − now_us` is asymmetric-clock-sensitive. For v1 we accept that — the tie-breaker is advisory, not safety-critical. If the estimate is unreliable, equal-hop paths get picked arbitrarily; that's the current behavior anyway.

### 4. `RoutingTable` authoritative; `ProximityGraph` is input + fallback

The data path already only consults `RoutingTable` via `send_routed`. Reroute should match.

Add `RoutingTable::lookup_alternate(dest, exclude_next_hop) -> Option<SocketAddr>` — returns the best (lowest-metric) entry for `dest` whose `next_hop != exclude_next_hop`. `ReroutePolicy::on_failure` calls this first when a peer dies; only if the table has no alternate does it fall back to `ProximityGraph::path_to()` to synthesize one and install it via `add_route_with_metric`.

Concretely:

```rust
// In ReroutePolicy::on_failure, per affected destination:
let alt_addr = self
    .routing_table
    .lookup_alternate(dest_id, failed_addr)
    .or_else(|| self.find_graph_alternate_for(failed_node_id, dest_id));
```

This collapses "two independent truths" into one: the table answers the question, and the graph is a derivation path that feeds the table.

## Implementation steps

1. **`ProximityGraph::edges` insert + sweep.** On `on_pingwave`, insert `(source_peer_node_id → origin_node_id)` with a timestamp. Add a sweep that drops edges older than `max_route_age`; call from the existing heartbeat-loop tick alongside `sweep_stale`.
2. **Origin self-check + `MAX_HOPS` cap.** In `mesh.rs` pingwave dispatch (around line 723): reject if `origin_nid == ctx.local_node_id` or if `pw.hop_count >= MAX_HOPS`. No route install, no re-broadcast.
3. **Split horizon in re-broadcast.** In the re-broadcast spawn (`mesh.rs:749-756`), before sending `fwd_bytes` to a peer, check `router.routing_table().lookup(origin_nid)`: if the next-hop address matches that peer's address, skip it.
4. **Latency EWMA on pingwave receipt.** Extend `ProximityNode` with a `latency_ewma_us: AtomicU32` per origin keyed by next-hop. Update on receipt with `α = 1/8` EWMA over `now_us.saturating_sub(pw.origin_timestamp_us)`.
5. **`RoutingTable::lookup_alternate(dest, exclude) -> Option<SocketAddr>`.** New accessor. Picks the best non-excluded entry; when metrics tie, consult the graph's EWMA for the tiebreak.
6. **`ReroutePolicy::on_failure` rework.** Per affected destination: `lookup_alternate(dest, failed_addr)` first; fall back to `find_graph_alternate_for` only if `None`.
7. **Docs.** New "Routing" subsection in `TRANSPORT.md` — one paragraph on the pingwave→table flow, one on the three loop-avoidance rules, one on the table-authoritative principle.
8. **Tests** (below).

## Tests

- **Unit (`route.rs`)** — `lookup_alternate` picks the lowest-metric alternate excluding a given next_hop; returns None if only the excluded route exists; returns None for a stale (age-out-eligible) sole entry.
- **Unit (`proximity.rs`)** — edge insert on pingwave; edge sweep on age-out; `path_to` returns a real 3-hop path after enough pingwave traversal; origin self-check drops the pingwave and installs no route; `MAX_HOPS`-exceeding pingwave drops and installs no route; latency EWMA updates on successive receipts.
- **Integration** — 4-node chain A-B-C-D with pingwaves propagating. After convergence:
  - A's routing table has `(D, via=B, metric≥3)`, `(C, via=B, metric=2)`.
  - A's `path_to(D)` returns `[A, B, C, D]`.
  - Split horizon: A does **not** re-broadcast D's pingwave back to B.
  - Killing B: A's routes for C and D reroute through an alternate if one exists; if not, both entries expire via `sweep_stale`.
  - Origin staleness: if D stops emitting pingwaves but B is still alive, A's route to D expires naturally via `sweep_stale`; A's route to B does not.
- **Regression** — origin self-echo: a pingwave with `origin_id == self_id` is dropped and does not replace the node's own routes.

## Risks and open questions

- **Count-to-infinity.** TTL + `MAX_HOPS` + split horizon + metric age-out bound it. If a partition stabilizes, routes converge; if it oscillates fast, queries may hit stale routes until `sweep_stale`. Documented expected behavior — not a bug.
- **Unbounded fan-out.** Pingwave flooding is O(peers²) per heartbeat cycle in a fully-connected mesh. At current 5 s heartbeat this is fine; larger meshes will want selective re-broadcast (the `on_pingwave` hook already returns `Option<EnhancedPingwave>` and can drop). Out of scope here; flagged.
- **Clock skew for latency tie-break.** `origin_timestamp_us − now_us` is asymmetric-clock-sensitive. The tie-breaker is advisory — unreliable estimates just degrade to "pick an arbitrary equal-hop path," which is the current behavior.
- **Trust model.** Malicious peers can inflate/deflate hop counts or forge origin ids to hijack routes. The origin self-check and `MAX_HOPS` cap defend against obvious abuse; a signed-pingwave protocol is separate work. For v1, assume mutually-trusted participants (PSK gates mesh entry).
- **Interaction with subnets.** Pingwaves are raw UDP (not channel traffic), so subnet-gateway visibility rules don't directly apply. Confirm during implementation that the gateway doesn't filter raw pingwaves; if it does, decide whether subnet-local topology is intentional.

## Summary

Not a new protocol — the finish work on the one that's already half-shipped. Populate `ProximityGraph::edges`, add three cheap loop-avoidance rules (origin self-check, `MAX_HOPS` cap, split horizon on re-broadcast), latency EWMA as a tie-break on the metric, and `RoutingTable::lookup_alternate` so reroute hits the table first. The routing table remains the authoritative source for `send_routed`; the graph is an input and a fallback path synthesizer. ~200 LoC of changes + ~150 LoC of tests. No wire-format change.
