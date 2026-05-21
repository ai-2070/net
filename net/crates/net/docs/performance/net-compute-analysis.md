# Performance Analysis: Compute (Scheduler, Groups, Load Balance, Daemon Host)

Supplemental to the unified report. Focuses on the compute runtime — the daemon-host hot path that processes events on behalf of user workloads, the per-event group-routing logic, the load balancer, and the scheduler. Items continue from #147.

The compute path is hotter than it looks. Every event delivered to a daemon hits the host's `deliver`, which runs causal-link bookkeeping per event. Every event routed through a fork/replica/standby group runs the load balancer's `select`. The load balancer's hot path turns out to be one of the most allocation-heavy paths in the entire codebase per event.

---

## 🔴 High-impact

### 148. `LoadBalancer::get_available_endpoints` walks every endpoint and clones per match — per event

**Location:** `behavior/loadbalance.rs:885-948`:

```rust
fn get_available_endpoints(&self, ctx: &RequestContext) -> Result<Vec<Arc<EndpointState>>, ...> {
    let mut available = Vec::new();
    let mut zone_matches = Vec::new();

    for entry in self.endpoints.iter() {        // <-- full DashMap walk per event
        let state = entry.value();
        if !state.is_available() { continue }       // atomic load
        if state.is_circuit_open(recovery_time) { continue }   // atomic load + clock
        if state.connections.load(Relaxed) >= max { continue }   // atomic load
        if !ctx.required_tags.is_empty() && !ctx.required_tags.iter().all(|t| state.tags.contains(t)) { continue }
        // ...
        available.push(Arc::clone(state));     // atomic refcount bump per match
    }
    // ...
}
```

This runs on **every routed event**. For a load balancer with 100 endpoints and 100K events/sec:

- 10M DashMap iterations/sec
- 30-40M atomic loads/sec (3-4 per endpoint × 100 endpoints × 100K events)
- 5-10M Arc refcount bumps/sec for survivors
- 2 fresh Vec allocations per event = 200K allocs/sec

This is the dominant cost on the compute-group routing path.

**Fix:**
Maintain a snapshot of available endpoints, updated only on health/circuit/enabled state changes. Routing reads it via `ArcSwap<Vec<Arc<EndpointState>>>`:

```rust
struct LoadBalancer {
    // existing fields ...
    available_snapshot: ArcSwap<Vec<Arc<EndpointState>>>,
}
```

Per `select()`: one atomic load (the ArcSwap), zero iteration, zero allocation. Health/circuit state changes update the snapshot off the hot path.

For zone-aware routing, stratify into `HashMap<Zone, ArcSwap<Vec<...>>>` so zone lookup is a single ArcSwap load.

**This is the single biggest per-event win on the compute path.**

### 149. `EndpointState::metrics()` does a RwLock read + full clone per call — called per endpoint per select for several strategies

**Location:** `behavior/loadbalance.rs:280-282`:
```rust
fn metrics(&self) -> LoadMetrics {
    self.metrics.read().clone()
}
```

Called from `select_least_latency`, `select_least_load`, `select_power_of_two`, `select_adaptive`. AND from every `Selection { ..., load_score: state.metrics().load_score() }` site — meaning **even RoundRobin pays the full metrics clone** just to populate an output field.

Per select with 100 endpoints + LeastLatency: **100 RwLock reads + 100 LoadMetrics clones per event**.

**Fix:**
- `LoadMetrics` is probably a small struct of `AtomicU64`s wrapped in a holder. Replace `metrics: RwLock<LoadMetrics>` with `metrics: LoadMetricsAtomic` where each field is an atomic. Reads become atomic loads with no clone.
- For the `Selection { load_score: ... }` field: only populate it when the strategy actually uses it. RoundRobin shouldn't pay this cost.

For 100K events/sec on a 100-endpoint LB, that's 10M RwLock-read + clone ops/sec eliminated.

### 150. `select_consistent_hash` rebuilds the full hash ring per event

**Location:** `behavior/loadbalance.rs:1131-1170`:
```rust
let mut ring: Vec<(u64, NodeId)> = self.hash_ring
    .iter()
    .map(|entry| (*entry.key(), *entry.value()))
    .collect();
ring.sort_unstable_by_key(|&(k, _)| k);

let idx = ring.partition_point(|&(k, _)| k < hash);

for i in 0..ring.len() {
    let (_, node_id) = ring[(idx + i) % ring.len()];
    if let Some(state) = endpoints.iter().find(|e| e.node_id == node_id) {  // O(N) per ring entry
        return Selection { ... };
    }
}
```

Per ConsistentHash select:
- Collect all ring entries (1600+ for typical 16-virtual-nodes-per-endpoint × 100 endpoints)
- Sort them
- Linear-scan endpoints to find a match per ring lookup

**This is O(N log N) per request when it should be O(log N).**

**Fix:** Maintain the ring as a pre-sorted `ArcSwap<Vec<(u64, NodeId)>>`, updated incrementally on endpoint add/remove. Routing is binary search on the loaded snapshot. Endpoint resolution: build a `HashMap<NodeId, usize>` index alongside the available endpoints so the find is O(1) instead of O(N).

For a hot consistent-hash workload (commonly used for cache affinity, session pinning), this is the difference between "scales to 100 endpoints" and "doesn't."

### 151. `Scheduler::pick_best_candidate` sorts when it should take the max

**Location:** `compute/scheduler.rs:474-494`:
```rust
let mut scored: Vec<(u64, f32)> = candidates.into_iter()
    .filter_map(|n| placement.placement_score(&n, artifact).map(|s| (n, s)))
    .filter(|(_, s)| s.is_finite())
    .collect();

scored.sort_by(|(a, sa), (b, sb)| {
    sb.partial_cmp(sa).unwrap_or(Ordering::Equal)
        .then_with(|| tie_break_compare(*a, *b, tie_break))
});

scored.first().map(|(n, _)| *n)
```

Full sort to take only the first element. For 1000 candidates that's O(N log N) when O(N) `max_by` would suffice.

This is the same pattern as #89 in the memories query layer. Same fix:
```rust
scored.into_iter().max_by(|(a, sa), (b, sb)| {
    sa.partial_cmp(sb).unwrap_or(Ordering::Equal)
        .then_with(|| tie_break_compare(*b, *a, tie_break))
})
```

Combined with #114 (per-candidate `with_caps` shard-lock acquisition during scoring), the scheduler's placement decision is currently doing significant work that could compress dramatically.

### 152. `GroupCoordinator::origin_hash_for_entity_id` is a linear scan called per routed event

**Location:** `compute/group_coord.rs:250-255`:
```rust
fn origin_hash_for_entity_id(&self, entity_id: &NodeId) -> Option<u64> {
    self.members
        .iter()
        .find(|m| m.entity_id_bytes == *entity_id)
        .map(|m| m.origin_hash)
}
```

Called by `route_event` per event routed through any compute group. `NodeId` is `[u8; 32]`, so each comparison is a 32-byte equality check.

For a group with 100 members at 100K events/sec: 10M × 32-byte comparisons/sec just for the origin_hash lookup after the load balancer picked an endpoint.

**Fix:** Maintain `entity_id_to_origin_hash: HashMap<NodeId, u64>` alongside `members`. Update on `add_member`, `remove_member`, `update_member_placement`. Lookup becomes O(1).

For groups with many members (replica groups, fork groups at scale), this is per-event waste compounding with #148's load balancer cost.

### 153. `DaemonHost::deliver` calls `horizon.encode()` per event even when there are no outputs

**Location:** `compute/host.rs:231`:
```rust
pub fn deliver(&mut self, event: &CausalEvent) -> Result<Vec<CausalEvent>, DaemonError> {
    self.horizon.observe(event.link.origin_hash, event.link.sequence);
    let outputs = self.daemon.process(event)?;
    self.stats.events_processed += 1;

    let horizon_encoded = self.horizon.encode();    // <-- always
    let mut causal_outputs = Vec::with_capacity(outputs.len());
    for payload in outputs {
        // ... uses horizon_encoded
    }
    // ...
}
```

`horizon.encode()` walks every entry in the horizon hashmap and computes `xxh3_64` per entry. For a horizon tracking 16 origins: 16 hash computations per `deliver()` call.

Most daemon events don't produce outputs (think: state updates, observations, filtering). When `outputs.is_empty()`, the horizon encode is pure waste — there's nothing to attach it to.

**Fix:** Skip the encode when outputs are empty:
```rust
let outputs = self.daemon.process(event)?;
self.stats.events_processed += 1;
if outputs.is_empty() {
    return Ok(Vec::new());
}
let horizon_encoded = self.horizon.encode();
// ... continue
```

For daemons with low output ratio (1 output per N inputs), saves (N-1)/N of the encode cost.

**Further fix:** Cache the encoded horizon, invalidate on `observe()`. Most observes target an origin that's already in the bloom (no bit change). Only re-encode on observed-new-origin.

### 154. `compute_parent_hash` allocates a fresh `Vec` per output event purely to concatenate before hashing

**Location:** `state/causal.rs:127-135`:
```rust
pub fn compute_parent_hash(prev_link: &CausalLink, prev_payload: &[u8]) -> u64 {
    let link_bytes = prev_link.to_bytes();
    let mut combined = Vec::with_capacity(CAUSAL_LINK_SIZE + prev_payload.len());
    combined.extend_from_slice(&link_bytes);
    combined.extend_from_slice(prev_payload);
    xxh3_64(&combined)
}
```

Per output event from any daemon: allocate a Vec, memcpy 32 bytes + the payload into it, hash, drop the Vec. For a daemon emitting 100K events/sec with 1KB payloads: 100K allocs/sec + 100MB/sec of memcpy bandwidth just for parent-hash computation.

The comment even acknowledges this: "For large payloads, use xxh3's incremental API if needed (future optimization)." But there's no size threshold — the alloc fires for every payload regardless of size.

**Fix:** Use streaming xxh3:
```rust
use xxhash_rust::xxh3::Xxh3;
let mut h = Xxh3::new();
h.update(&prev_link.to_bytes());
h.update(prev_payload);
h.digest()
```

Zero allocation, no memcpy. Slightly more streaming overhead than one-shot for tiny inputs, but compensates with zero alloc — net win.

Same pattern as #92 in the CortEX checksum code; same fix.

### 155. `LoadBalancer::try_record_request` takes a Mutex + clock syscall per successful reservation

**Location:** `behavior/loadbalance.rs:305-320`:
```rust
fn try_record_request(&self, max_connections: u32) -> bool {
    let reserved = self.connections.fetch_update(...);
    if reserved {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        *self.last_selected.lock() = Instant::now();    // <-- Mutex + clock
    }
    reserved
}
```

Per successful routing decision: a parking_lot Mutex lock + `Instant::now()` syscall.

**Fix:** `last_selected: AtomicU64` storing nanos since epoch (or a coarse-clock tick). Atomic store, no lock. The Mutex is being used as cell storage, not for mutual exclusion.

For 100K successful selections/sec: 100K lock+unlock pairs eliminated, 100K clock reads eliminated.

## 🟡 Medium-impact

### 156. `Scheduler::find_migration_targets` clones the daemon filter + allocates a String per call

**Location:** `compute/scheduler.rs:208`:
```rust
const MIGRATION_TAG: &str = "subprotocol:0x0500";
let combined = daemon_filter.clone().require_tag(MIGRATION_TAG.to_string());
```

`daemon_filter.clone()` is a deep clone of `CapabilityFilter` (Vec of tags/models/tools, HashSet, etc). `MIGRATION_TAG.to_string()` heap-allocates from a static.

Migration placement is control-plane (not per-event), so frequency is low. But the pattern is wrong:
- `CapabilityFilter` could have `require_tag(impl Into<Cow<'static, str>>)` accepting static strs without alloc.
- Or `find_migration_targets` could take a pre-built filter from the caller and reuse across migration attempts.

### 157. `Scheduler::place_with_locality` double-allocates on the drained path

**Location:** `compute/scheduler.rs:138-146`:
```rust
let candidates: Vec<u64> = if local_drained {
    self.capability_index.query(filter)
        .into_iter()
        .filter(|&id| id != self.local_node_id)
        .collect()
} else {
    self.capability_index.query(filter)
};
```

The drained path: `query` allocates a Vec, then `into_iter().filter().collect()` allocates another Vec just to drop one item (the local node).

**Fix:** Add `CapabilityIndex::query_excluding(filter, &exclude_set)` that filters during the inner walk. Saves one Vec allocation.

Same pattern in `place_with_spread` (`compute/group_coord.rs:268`): if primary placement is excluded, calls `query_candidates` again — second full index query when the first one already produced the candidate set.

### 158. `GroupCoordinator::mark_healthy` and `mark_unhealthy` linear-scan members

**Location:** `compute/group_coord.rs:148-163`:
```rust
pub fn mark_unhealthy(&mut self, index: u8) {
    if let Some(member) = self.members.iter_mut().find(|m| m.index == index) {
        // ...
    }
}
```

Linear scan by index. Members are stored in `Vec<MemberInfo>` indexed by `u8`. If `index` is sequential and dense (which it likely is — `for index in current..n` at the scale_to site), this could be direct array indexing: `members.get_mut(index as usize)`. O(1) instead of O(N).

Cold-ish path (health changes are infrequent), but trivial fix.

### 159. `RecoveryRegistry::try_run_all` allocates per tick

**Location:** `compute/mod.rs:154-193`. Per recovery tick (~1Hz):
- `mem::take` to swap out the handler vec → allocates a new empty one
- `Vec::with_capacity(handlers_to_run.len())` for survivors
- `Vec::new()` for the recovered slot list
- Each handler is called via `catch_unwind` (overhead)
- Merge survivors back

For a low-frequency tick this is fine. If recovery becomes per-event for some reason, it'd matter.

### 160. `LoadBalancer::select_weighted_round_robin_at` recomputes `total_weight` per call

**Location:** `behavior/loadbalance.rs:982`:
```rust
let total_weight: f64 = endpoints.iter().map(|e| e.effective_weight()).sum();
```

Per WeightedRoundRobin select, iterates all endpoints, calls `effective_weight()` per endpoint (which reads `is_enabled()` atomic + `health()` RwLock read). Sum re-computed per event.

**Fix:** Cache `total_weight: AtomicF64` (or `AtomicU64` of `f64::to_bits()`), updated incrementally on weight/health changes. Per WRR select: one atomic load instead of N RwLock reads + sum.

### 161. `LoadBalancer::select_random` and `select_weighted_random` likely use thread_rng per call

Didn't look at the body but the pattern across the codebase suggests `rand::thread_rng()` per select. `thread_rng` is thread-local but still hits a TLS slot per call.

For high-rate random LB selection, instantiate a per-LB `SmallRng` seeded once. Per-call: pure userspace RNG step. Worth checking the actual code if Random is a configured strategy in production.

### 162. `DaemonHost::deliver` runs `current_timestamp()` per output event via `CausalEvent::received_at`

**Location:** `state/causal.rs:185`:
```rust
let event = CausalEvent {
    link: next_link,
    payload: payload.clone(),
    received_at: current_timestamp(),    // <-- per output
};
```

Per output event from a daemon. Same coarse-clock pattern (#33, #66, #115, #135, #137, #155).

### 163. `LoadBalancer::select` retry loop pays the full filter cost per attempt

**Location:** `behavior/loadbalance.rs:764`. The retry loop runs `get_available_endpoints(ctx)` up to 4 times if reservation races. Each retry pays the full DashMap walk + filter cost (the #148 cost).

If #148 is fixed (snapshot-based filtering), retries become cheap. If not, contended scenarios pay 4× the per-event cost.

## 🟢 Low-impact / cleanup

### 164. `CapabilityFilter::clone` in `find_migration_targets` could be a borrowed builder

Already covered in #156. Listed for completeness.

### 165. `select_least_latency` and `select_least_load` walk all endpoints to find min

Standard linear min over N. Endpoints don't expose a sorted view so this is unavoidable without a separate heap. For small N (typical), it's fine. For large N with these strategies, a maintained min-heap would help, but probably not worth the complexity.

### 166. `place_with_spread` returns `PlacementDecision { reason: FirstMatch }` even when it ran the full exclusion-filter search

`compute/group_coord.rs:267-275`. The reason field doesn't distinguish "first match" from "filter-narrowed first match." Cosmetic — affects observability not perf.

### 167. `GroupCoordinator::healthy_count` does a linear scan + saturation cast

`compute/group_coord.rs:232-238`. Cold accessor (control plane). Could cache via incremental update on health-change. Probably not worth it.

### 168. `LoadBalancer::endpoints` is a DashMap; iteration order is unspecified

Every LB strategy that iterates `endpoints` (which is all of them via `get_available_endpoints`) gets non-deterministic ordering. RoundRobin's deterministic step is computed via a separate counter, so this works — but if anyone added "iterate in insertion order" logic by accident, it'd be subtly broken. Worth a comment-level audit, not a perf item.

### 169. `select_consistent_hash` `endpoints.iter().find(|e| e.node_id == node_id)` is O(N) per ring entry

Already covered in #150. The fix there subsumes this.

### 170. Causal `CausalEvent::clone` is implicit in many paths

`state/causal.rs:138` — `#[derive(Clone)]`. Per-event clones likely happen at delivery boundaries. The `payload: Bytes` is cheap; the `link: CausalLink` is 32 bytes Copy; `received_at: u64` is Copy. So a CausalEvent clone is ~40 bytes of memcpy + one Bytes refcount bump. Not bad, but if it's per-event in a hot loop, worth checking call sites.

---

## What I'd actually do

The compute-path findings cluster into a clear hierarchy:

**Top 3 (transformative on per-event compute routing):**

1. **#148 — snapshot-based available endpoints in LoadBalancer.** Removes a full DashMap walk + 4× atomic ops per endpoint + 2 Vec allocs per event. Probably 5-15× speedup on the LB hot path for high-endpoint deployments.

2. **#149 — atomic LoadMetrics instead of RwLock<LoadMetrics>.** Removes a RwLock read + LoadMetrics clone per endpoint per select. Compounds with #148 — once you have the snapshot, the per-endpoint metrics read is the next bottleneck.

3. **#150 — pre-sorted hash ring with O(log N) lookup.** Only matters if ConsistentHash is the configured strategy, but when it is, this is the difference between "scales" and "doesn't."

**Next tier (per-event daemon-host cost):**

4. **#153 — skip horizon encode when there are no outputs.** Per-event win for daemons with low output ratio.

5. **#154 — streaming xxh3 in compute_parent_hash.** Per-output-event allocation eliminated.

6. **#152 — entity_id → origin_hash map in GroupCoordinator.** Per-routed-event lookup fix.

**Wins that depend on whether compute is hot for your users:**

If users run compute workloads through your scheduler at high event rates (the architectural pitch beyond just RPC), these items matter a lot. If compute is a niche feature, they're nice-to-have.

**Items I'd skip:**

The migration / placement items (#156, #157, #158, #166, #167) are all cold-path. They're correctness-grade or observability-grade, not perf-grade.

---

## Compounding with prior findings

The compute path doesn't exist in isolation. Several items here interact with previously-flagged findings:

- **#148 (LB snapshot)** removes work that gets compounded by **#107 (session NodeId resolution)** and **#106 (routing-table lookup)**. Per-event compute routing currently pays all three.
- **#149 (atomic metrics)** is the same pattern as **#11 (RedexIndex Arc<HashSet>)** and **#96 (Arc<Memory>)** — "clone the inner value to read" is a recurring anti-pattern that snapshot/Arc fixes.
- **#151 (max_by instead of sort)** is the same pattern as **#89 (memories query top-K)** — top-K via sort is a recurring sub-optimization.
- **#154 (streaming xxh3)** is the same pattern as **#92 (cortex checksum)** — allocating to concatenate-then-hash is a recurring anti-pattern.

**Cross-cutting fix:** A "Arc-wrap-and-snapshot" pattern applied uniformly across LB endpoints, capability index entries, memories/tasks state, and replication metadata would eliminate the per-read-clone cost in every subsystem at once. The diffs are mechanical and similar across all sites.

---

## Honest expectation

The compute path is where I'd expect the biggest unrealized wins for users running heavy workload orchestration. Specifically:

- **High-event-rate compute groups** (many events/sec routed through fork/replica groups): #148, #149, #152 compound. Likely 3-10× on the per-event routing cost.
- **ConsistentHash users**: #150 alone is potentially 100× on selection latency at 100+ endpoints.
- **Daemons with low output ratio** (filtering, observing, state-update workloads): #153 cuts deliver() cost meaningfully.
- **Heavy chain producers** (daemons that emit GB/s of chained events): #154 + #162 cut per-output allocation + clock cost.

For users who DON'T run compute workloads — pure pub/sub or RPC users — none of this matters. The compute subsystem only fires when daemons + groups are used.

If compute is part of your product pitch (workload orchestration, state-replicated services, daemon scheduling), this section probably contains the highest-leverage items in the entire audit. If it's a legacy or niche subsystem, skip.
