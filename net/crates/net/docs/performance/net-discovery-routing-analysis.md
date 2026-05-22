# Performance Analysis: Discovery + Routing Path

Supplemental to the unified report. Focuses on the per-call discovery, routing, and dispatch path — the work that exists *because* the system does automatic service discovery + mesh routing rather than the point-to-point connections gRPC users assemble themselves.

This is the architectural-advantage area: optimizing here protects the substrate-derived gap vs gRPC + Consul + service mesh stacks. Items numbered continuing from the unified report (#105+).

---

## ✅ Fixed

| # | Item | Notes |
|---|------|-------|
| 108 | `dispatch_packet` per-packet session→NodeId resolution → per-session cache | New `cached_node_id: AtomicU64` on `NetSession` (sentinel `0` = unresolved). `dispatch_packet`'s RPC hook tries the cache first via `session.cached_node_id()` — single Relaxed atomic load. Miss runs the legacy chain (`addr_to_node` lookup + session_id verification, then full peer scan on stale addr) and calls `session.cache_node_id(nid)` to publish for subsequent packets. For high-RPC-rate workloads on persistent sessions, drops the per-packet cost from "2 DashMap lookups + comparison + possible O(N) scan" to one atomic load. Pinned by `cached_node_id_returns_none_until_published_then_caches` (covers the unresolved → cached transition and the `0` sentinel no-op). |
| 109 | `publish_many` events `Vec` cloned per spawned task → `Arc<[Bytes]>` shared | The `BestEffort` / `Collect` paths now hoist the events into an `Arc<[Bytes]>` once and clone the `Arc` per spawned task. Pre-fix a 100-subscriber × 1000-event broadcast did 100 `Vec` allocations + 100K `Bytes` refcount bumps; post-fix it's 1 `Vec` alloc + 1K `Bytes` bumps + 100 `Arc` bumps. The spawned task derefs `Arc<[Bytes]>` to `&[Bytes]` at the `publish_to_peer` call site — no signature change required. |
| 105 | `publish` two sequential `retain` passes → single fused filter | The subnet-visibility filter and the auth-guard + token-expiry filter are now one `retain` closure. Cheapest check (subnet) runs first and short-circuits before any auth-cache probing. Each peer hits its DashMap lookups (`peer_subnets`, `auth_guard.check_fast`, `peer_entity_ids`, `token_cache`) in cache-locality order rather than two passes over the whole Vec. The Vec is walked once not twice (eliminating the inner `Vec::retain` shift cost) and the publish-path's per-peer work scales linearly in one pass. Semantics unchanged: same per-peer admit/deny verdicts and same inline `revoke_channel` side effect on token denial. |
| 110 | `CapabilityIndex::build_candidate_set` `HashSet<u64>` clones + intersection-as-collect → `Vec<u64>` + in-place `retain` | Pre-fix every filter clause that hit an indexed dimension did either `tag_nodes.clone()` (first match) or `c.intersection(&set).copied().collect()` (subsequent matches) — both allocate a fresh `HashSet<u64>` per clause. For a discovery query "tag=worker AND model=llama3 AND tool=python" against 1000-worker × 500-llama3 × 800-python that's ~5 HashSet allocs per query, each holding hundreds-to-thousands of u64s. Switched the working set + return type to `Vec<u64>`: first matched filter materializes via `.iter().copied().collect()` (one Vec alloc, same as before); each subsequent indexed clause is a borrow + `Vec::retain(|n| index.contains(n))` — no new container allocated. The temporary `HashSet` for the multi-value model/tool union stays (it's the right structure for dedup-on-extend) but the intersection against `candidates` swaps to retain. Inverted-index entries are already `HashSet<u64>` internally so the initial materialization is unique; `retain` preserves that invariant. All 3 call sites (`query`, `find_best`, `find_nodes_scoped`) consume the result via `.into_iter()` which works equally well over `Vec<u64>`. Pinned by `build_candidate_set_intersection_semantics_preserved_after_vec_refactor`: exercises A∩B, A∩C, A∩m1 (tag×model), the no-matches short-circuit, and the dedup invariant. |
| 118 | `dispatch_recipients` `out.contains(&picked)` O(N) → cap-and-grow `HashSet` dedup | Linear scan stays on the fast path for small fan-outs (the typical case where the linear walk beats hash+probe on cache locality). Once `out` crosses `DEDUP_HASHSET_THRESHOLD = 32` the function seeds a `HashSet<u64>` from the current `out` and switches subsequent membership checks to O(1). The structural invariant from `add` (a peer is either a broadcaster or in exactly one queue group, never both / never two groups) means the dedup currently never rejects, but the defensive check stays so a future invariant break can't silently fan-out duplicate packets. Pinned by `dispatch_recipients_dedup_holds_across_hashset_promotion` (100 distinct subscribers exercising both paths in one call). |

---

## The hot paths in scope

For a discovery-routed RPC from caller to callee, the per-call work that gRPC doesn't do:

1. **Publisher-side discovery + filtering** (`mesh.rs::publish`, `publish_many`): roster lookup, subnet visibility filter, ACL/token check, per-peer dispatch.
2. **Per-subscriber send** (`publish_to_peer`): session resolution, partition check, stream open, credit acquire, packet build, routing table lookup, socket send.
3. **Inbound packet receive** (`dispatch_packet` → RPC dispatcher hook): magic check, parse, channel-hash dispatch lookup, session NodeId resolution, fold delivery.
4. **Routing table lookup** (`RoutingTable::lookup`): per outbound packet, also per inbound packet for return-path checks.
5. **Capability index queries** (`CapabilityIndex::query`, `find_best`): per workload placement decision (less hot, but on the discovery slow path).
6. **Placement scoring** (`StandardPlacement::placement_score`): per candidate × axes, called by schedulers + load balancers.

A typical RPC call traverses 1-4 on the publish side, 3-4 on the receive side, and 5-6 only when membership or placement changes (less hot but important for "discovery overhead vs gRPC").

---

## 🔴 High-impact

### 105. `publish` does multiple sequential `retain` passes over the subscriber list

**Location:** `mesh.rs:6498-6614`. Three independent retain passes:

```rust
let mut subscribers = self.roster.dispatch_recipients(publisher.channel());  // alloc Vec
subscribers.retain(|peer_id| { /* subnet visibility per peer */ });           // pass 1
subscribers.retain(|peer_id| { /* auth_guard + token check per peer */ });    // pass 2
// then per-peer publish_to_peer
```

For each subscriber, the publish path does:

| Pass | DashMap ops per peer | What it checks |
|------|---------------------|----------------|
| dispatch_recipients | 1-2 (roster lookup + broadcasters/groups) | who subscribes |
| subnet retain | 1 (`peer_subnets.get`) | visibility |
| auth retain | 1-3 (auth_guard fast/full + token cache) | ACL + token |

For 100 subscribers: ~400-600 DashMap accesses per publish, plus the publish_to_peer chain (more below).

**Fix:** Single-pass filter+collect:
```rust
let subscribers: SmallVec<[u64; 16]> = self.roster
    .dispatch_recipients_iter(publisher.channel())   // borrowed iter, no Vec alloc
    .filter(|peer_id| visible(*peer_id) && authorized(*peer_id))
    .collect();
```

Each peer hits the DashMaps once (with locality on the shard cache lines if you order checks to hit the same shards first). For 100 subscribers, drops from ~500 DashMap ops to ~200.

Also: `dispatch_recipients` allocates a `Vec<u64>` even on the fast "broadcast-only" path. An iterator variant for the common case avoids the Vec entirely.

**This is the single biggest per-publish win on the discovery path.** Publishing is the operation gRPC doesn't do at all (their users hold connections directly), so every cycle saved here widens the architectural gap.

### 106. `publish_to_peer` does 3 DashMap lookups + 1 routing-table lookup per subscriber

**Location:** `mesh.rs:6724`. Per subscriber:
1. `self.peers.get(&peer_node_id)` — DashMap
2. `self.partition_filter.contains(&dest_addr)` — DashMap-ish
3. `session.open_stream_with()` — internal locks
4. `session.try_acquire_tx_credit_guard()` — atomic chain (covered in #42)
5. `session.thread_local_pool().get()` — covered in #17
6. Packet build
7. `self.router.routing_table().lookup(peer_node_id)` — DashMap + `Instant::now()` (covered in #33)
8. `socket.send_to().await`

Step 7's `lookup` happens AFTER the peer was already resolved via `peers.get()`. The `peers` entry at step 1 already gave you `addr`. The routing table lookup overrides this with a next-hop if the route goes through an intermediate, but for direct peers (the common case), the routing table answer equals `dest_addr` — redundant lookup.

**Fix:** Routing-table lookup is only meaningful when the destination differs from the peer's known direct addr. Cache "is this peer reached directly" as a bit on `PeerInfo`, skip the routing-table lookup when it's true. For mostly-direct topologies (typical), this cuts a DashMap access per packet sent.

For mesh routing topologies where indirect routes matter, structure the peer entry to hold an `Arc<RouteCache>` that gets invalidated on route changes — read with one atomic load instead of a DashMap lookup.

### 107. `dispatch_packet` falls back to a full peer-table linear scan to resolve session_id → NodeId

**Location:** `mesh.rs:4145-4158`:
```rust
let from_node = ctx.addr_to_node.get(&session.peer_addr())
    .and_then(|nid| {
        ctx.peers.get(&*nid).and_then(|p| {
            (p.value().session.session_id() == session_id).then_some(*nid)
        })
    })
    .or_else(|| {
        ctx.peers.iter()                                    // <-- O(N) full peer scan
            .find(|e| e.value().session.session_id() == session_id)
            .map(|e| e.value().node_id)
    });
```

Fast path: two DashMap lookups + a session_id comparison. Slow path: **linear scan of the entire peer table**, comparing session_ids one by one.

The fallback fires when `addr_to_node` is stale relative to `peers` — which happens during membership churn (peer reconnects on a new address, NAT rebind, address-family flip). Exactly the conditions where you most need predictable latency.

**Fix options:**
- **Reverse index:** maintain a `session_id → node_id` DashMap. Keep it in sync with `peers` on insert/remove. One DashMap lookup, no scan.
- **Stale-tolerant fast path:** if `addr_to_node` was just populated (per session), trust it. Track a small generation counter so the validation step can short-circuit.

The DashMap reverse index is the cleaner fix. It's a small write-amplification (every peer insert/remove updates one extra map) for a huge tail-latency win during churn.

This matters especially because the situation it triggers in (membership churn, NAT rebind) is precisely "what makes mesh discovery hard, that gRPC's connection model doesn't have to deal with." Slow tail latency under churn is the negative pitch against your architecture.

### 108. `dispatch_packet` does session_id resolution PER PACKET, not per session

**Same location, same code.** The `from_node` resolution runs on **every inbound RPC packet**, not once per session. The session itself is stable — once you've resolved `session → node_id` for an established session, that mapping doesn't change.

**Fix:** Cache `node_id` on the `Session` struct itself. Resolved once on session handshake. Per-packet hot path: `session.cached_node_id()` — single load, no DashMap, no scan.

For high-RPC-rate workloads on persistent sessions (the common case for service-meshed apps), this drops the per-packet discovery cost from "2 DashMap lookups + comparison + possible O(N) scan" to "one atomic load."

This is probably the single biggest per-packet win on the inbound side.

### 109. `publish_many` clones the events Vec per subscriber in the spawn closure

**Location:** `mesh.rs:6656`:
```rust
for peer_id in subscribers {
    let permit = Arc::clone(&sem);
    let events_owned: Vec<Bytes> = events.to_vec();           // <-- clone per peer
    let fut = async move {
        // ...
        self.publish_to_peer(peer_id, ..., &events_owned).await
    };
    handles.push(fut);
}
```

For 100 subscribers × 1000-event batch: 100 Vec allocations + 100K Bytes refcount bumps per publish, when an `Arc<[Bytes]>` would be one allocation + 100 Arc clones.

**Fix:** Hoist the Vec once: `let events_shared: Arc<[Bytes]> = events.iter().cloned().collect::<Vec<_>>().into();` then `let events_owned = Arc::clone(&events_shared);` per spawn.

For broadcast-heavy workloads (large fanout), this is meaningful.

### 110. `CapabilityIndex::build_candidate_set` clones whole HashSets to intersect

**Location:** `behavior/capability.rs:3192-3263`:
```rust
match self.by_tag.get(tag) {
    Some(tag_nodes) => {
        candidates = Some(match candidates {
            Some(c) => c.intersection(&tag_nodes).copied().collect(),  // alloc
            None => tag_nodes.clone(),                                   // alloc
        });
    }
    None => return Some(HashSet::new()),
}
```

Per query, for each filter clause:
- Clone the inverted-index entry's full HashSet (could be hundreds of node ids).
- Intersect with running candidates → another HashSet alloc.

For a query "tag=worker AND model=llama3 AND tool=python" with 1000 worker nodes, 500 llama3 nodes, 800 python nodes: 3 HashSet clones + 2 intersection allocations = 5 HashSet allocs per query, totaling several thousand node ids worth of bookkeeping.

Then `query()` re-validates each candidate via `nodes.get()` (DashMap lookup per candidate).

**Fix:**
- Start with the smallest set as the candidate base; subsequent intersections walk the candidate (small) and probe the large sets via `contains`. O(min × log max) instead of O(sum).
- Skip the HashSet allocation entirely: collect candidate node ids into a `SmallVec<[u64; 32]>` once smallest is identified, then filter with `contains` against the index entries (no clones).
- Cache the cardinality of each inverted-index entry to pick smallest-first cheaply.

For high-rate discovery queries, the alloc churn dominates.

---

## 🟡 Medium-impact

### 111. `query` re-validates every candidate via DashMap lookup

**Location:** `behavior/capability.rs:3387-3395`. After `build_candidate_set` returns N candidates, the function does `nodes.get(node_id).map(|n| filter.matches(&n.capabilities))` per candidate. N DashMap lookups + N filter evaluations per query.

The doc-comment explains why: inverted indices update non-atomically with `nodes`, so a stale index entry can list a node that no longer matches. The re-check closes the window.

**Fix:** The window is very small (between `remove_from_indexes` and `add_to_indexes` during re-announcement). Most calls find no staleness. Options:
- Add a fast-path counter: `index.version() == candidate.index_version` → skip re-check.
- Or use the existing `find_best` pattern (line 3406): single-pass scoring that already merges the lookup. Apply same pattern to `query`.

### 112. `find_nodes_scoped` does the same work twice

**Location:** `behavior/capability.rs:3449`:
```rust
let base = self.query(filter);                            // N DashMap lookups + matches
base.into_iter()
    .filter(|&node_id| {
        let Some(caps) = self.get(node_id) else { return false };  // N MORE DashMap lookups
        // ... scope filter using caps
    })
```

`query` already fetched and validated; `find_nodes_scoped` fetches again. For a scoped query against 100 candidates: 200 DashMap lookups.

**Fix:** Fold the scope filter into `query`'s closure, or expose a `query_with_filter` that takes the scope predicate alongside the capability filter and does it all in one pass.

### 113. `StandardPlacement::placement_score` calls 7 axis functions per candidate

**Location:** `behavior/placement.rs:597-614`. For each candidate, compute:
- scope, proximity, intent, colocation, resource, anti_affinity, custom = 7 axis evaluations

Each one reads from `target_caps` (the borrowed CapabilitySet under the index shard lock). Most axes are tag set membership checks — typically fast, but each does a HashSet lookup.

For "place this daemon" decisions over 1000 candidates: 7000 axis evaluations + 7000 hash lookups against the candidate's tags.

**Fix:** Most axes have early-exit conditions (`if self.scope_filter.is_none() { return 1.0 }`). Hoist these checks out of the per-candidate loop — if axis is disabled, compose with 1.0 unconditionally for every candidate, no need to call the function.

```rust
// Pre-compute which axes apply for this artifact + this placement config:
let axes_to_evaluate: SmallVec<[AxisFn; 7]> = self.applicable_axes(artifact);
// Per candidate, only evaluate applicable axes:
let score = axes_to_evaluate.iter().map(|f| f(&target_caps, target, artifact)).product();
```

For deployments using only 2-3 axes (typical), this cuts per-candidate work in half or better.

### 114. `placement_score` re-fetches `with_caps` per candidate via DashMap shard lock

**Location:** `behavior/placement.rs:536`. `self.index.with_caps(*target, |target_caps| ...)` acquires the index's per-shard read lock for each candidate.

For a placement decision over 1000 candidates, that's 1000 shard-lock acquire+release pairs. Most shards have 16-64 nodes; you'll repeatedly bounce between the same shards.

**Fix:** Batch by shard. Group candidate node_ids by which shard they live in (DashMap exposes `determine_map`). For each shard, acquire the lock once, evaluate all candidates in that shard, release. Cuts lock ops from O(candidates) to O(shards) — typically 10-100× fewer.

This is bigger work than other items but pays off for scheduler workloads doing thousands of placement evaluations.

### 115. `RoutingTable::lookup` calls `r.updated_at.elapsed()` per packet

**Location:** `route.rs:538-544` (also covered in #33 of unified report; reiterated here for the discovery-path context):
```rust
self.routes
    .get(&dest_id)
    .filter(|r| r.active && r.updated_at.elapsed() <= max_age)
    .map(|r| r.next_hop)
```

`Instant::elapsed()` = `Instant::now()` + subtract. Called per outbound packet. At GB/s wire rates, this is the single most-called clock-reading operation in the entire system.

**Fix:** Coarse-clock pattern. Background ticker (1ms) updates `route_freshness_epoch: AtomicU64`. Each route stores `installed_at_epoch: u64`. Lookup compares `current_epoch - installed_at_epoch <= max_age_epochs` — pure atomic loads, no clock syscall.

For mesh deployments doing heavy fan-out + forwarding, this alone might be 5-10% of wire-path CPU.

### 116. `subnet_visible` check + `peer_subnets.get` per subscriber per publish

**Location:** `mesh.rs:6515-6522`. Even when all peers are in the same subnet (most common deployment), every publish iterates and re-checks.

**Fix:** Per-channel cache of "all subscribers visible under current visibility config." Invalidate on:
- Channel config change (visibility flip)
- Subscriber add/remove
- `peer_subnets` change for any current subscriber

Steady-state publish: zero subnet-visibility work. Cache invalidation is "on the slow path that changes membership," not the per-publish path.

### 117. `auth_guard` lookup chain per subscriber per publish

**Location:** `mesh.rs:6571-6614`. Per peer, per publish:
- `subscriber_origin_hash(peer_id)` — compute
- `auth_guard.check_fast(origin, channel_hash)` — bloom + verified cache
- On `Allowed`: `auth_guard.is_authorized_full(origin, channel_name)` — exact ACL check
- If `require_token`: `peer_entity_ids.get(peer_id)` (DashMap) + `token_cache.check()` (DashMap)

Best-case: bloom hit + verified cache hit + no token = 2 DashMap-ish ops per peer per publish.
Worst-case: 4-5 DashMap ops per peer per publish.

For 100 subscribers: 200-500 DashMap-ish ops just for auth, per publish.

**Fix:** Cache the verdict per `(channel_hash, peer_id)` keyed on roster generation. Most steady-state publishes hit the cache with O(1). Invalidate on revoke + token expiry sweep. AuthGuard already has the bloom for negative lookups; this adds a positive-side cache to skip the full check.

### 118. `dispatch_recipients` `out.contains(&picked)` is O(N) per queue-group

**Location:** `channel/roster.rs:145`. For each queue group, scan the entire `out` Vec to check for dup. For 50 queue groups + 50 broadcasters, that's O(N²) = 2500 comparisons per publish.

**Fix:** For small N, this beats a HashSet. But cap-and-grow: when N > 32, switch to HashSet-backed dedup. Hot small case stays fast; large case scales linearly.

### 119. `register_rpc_inbound` and inbound lookup use u16 wire-bucketed DashMap with linear scan per bucket

**Location:** `mesh.rs:4093` (lookup) + 4722 (registration). The wire `channel_hash` is u16 = 65536 buckets. Each bucket is a `Vec<(ChannelHash, RpcInboundDispatcher)>`.

Lookup: DashMap get → bucket vec → linear scan for canonical match. For typical deployments (few thousand channels), bucket collisions are rare → single-entry vec → no scan.

The current implementation is already optimized for the common case (the `Snapshot::Single` fast path). Cleanup-grade.

Worth noting: the canonical match scan is `*existing_canonical == channel_hash` (u64 compare). Even worst-case 16 entries in a bucket = 16 u64 compares. Negligible.

### 120. `publish_to_peer` always calls `session.open_stream_with(stream_id, reliable, 1)` per send

**Location:** `mesh.rs:6753`. `open_stream_with` is presumably idempotent (creates if not present, no-op if open), but it's called per publish regardless. If the call involves a Mutex acquire or atomic CAS to check existence, that's per-publish overhead.

**Fix:** Cache "stream X is open with reliability Y" on the `ChannelPublisher` itself. First publish opens the stream and sets the cache; subsequent publishes skip the call.

Requires plumbing per-publisher state but eliminates an internal lock op per send.

### 121. `cfg_snapshot` clones the channel config per publish

**Location:** `mesh.rs:6454`:
```rust
let cfg_snapshot = self.channel_configs.as_ref().and_then(|cr| {
    cr.get_by_name(publisher.channel().name().as_str())
        .map(|c| c.clone())
});
```

`ChannelConfig` is a struct with likely several fields including `Option<Vec<Capability>>` for `publish_caps`. Per publish, full deep clone.

**Fix:** Store config as `Arc<ChannelConfig>` in the registry. Returns `Arc<ChannelConfig>` from `get_by_name` — one atomic refcount bump per lookup.

Or: change the publisher to hold an `ArcSwap<ChannelConfig>` updated by the channel registry on config change. Publish reads via `.load()` — single atomic load.

---

## 🟢 Low-impact / cleanup

### 122. `from_graph_id` conversion per pingwave forward decision

`mesh.rs:2855`. Pingwave forwarding is heartbeat-frequency, not per-RPC. Fine.

### 123. `add_route_with_metric` re-acquires the Instant via `Instant::now()`

`route.rs:509`. Already covered in clock-pattern items.

### 124. `dispatch_recipients` allocates a fresh Vec even when only broadcasters exist

`channel/roster.rs:141`. Most publishes go to broadcasters only — return an iterator over `broadcasters` for that case, allocate only when queue groups exist.

### 125. `members()` and `dispatch_recipients()` are almost the same code, in two places

`channel/roster.rs:378-399`. Refactor.

### 126. `is_subscribed` walks the peer's full channel set linearly

`channel/roster.rs:448-453`. For a peer subscribed to many channels, every `is_subscribed` check scans the set. DashSet contains is O(1), so this should already be fine; depends on what `set.contains(channel)` actually dispatches to.

### 127. `set_max_route_age` stores `as_nanos() as u64` per call

`route.rs:594`. Cold (operator-config path).

---

## What I'd actually do with this

If "protect the architectural advantage" is the goal, the three items that matter most:

1. **#108 — cache `node_id` on `Session` itself.** Per-packet inbound discovery cost goes from "DashMap chain + possible O(N) scan" to one atomic load. This is the highest-frequency item on the per-packet path.

2. **#105 + #109 — single-pass subscriber filtering + Arc-shared events Vec.** Publish-side fan-out is the big "we do something gRPC doesn't" cost. Halving the DashMap ops here meaningfully tightens the comparison.

3. **#107 — reverse session_id index.** Membership churn / NAT rebind is the worst-case scenario for "automatic discovery" pitches; eliminating the O(N) scan tail makes the architecture more defensible.

After those:

4. **#106 (skip routing-table for direct peers)** and **#117 (auth verdict cache)** — both per-subscriber-per-publish wins, both medium-effort.

5. **#114 (batch placement by shard)** — only matters if scheduler is on the user-visible path; for control-plane work it's nice to have.

6. **#110 + #111 + #112** (capability index intersection + revalidation) — important if discovery queries are frequent. For "subscribe once, publish many" workloads, less important than the publish-path items.

---

## Honest expectation on impact

These items are protecting an already-fast system on a path that's structurally non-zero. Unlike the easy wins in the original report (most of which were "you accidentally have an O(N²) here"), these are tightening already-good code.

If you implement #105, #107, #108, #109: per-publish + per-packet discovery overhead probably drops 30-50%. Whether that's a visible perf number depends on what fraction of total time is currently in this layer.

For workloads where users do "lots of small RPCs against a moderately-churning mesh" (the canonical "we replace gRPC + service mesh" pitch), this is probably the single most valuable area to optimize because it's the area where your architectural choice has the most overhead to amortize.

For workloads where users do "establish session, push GB through it" (streaming-heavy), most of these items don't fire often enough to matter — the wire path dominates and that's mostly UDP/syscall-bound.

So: **if you care about p99 RPC latency under membership churn, this is the work to do.** If you care about steady-state throughput on stable topologies, the original report's items have better ROI.
