# Scheduler — Gang-Claim Arbitration + Task Lifecycle

Read this when the user wants to **atomically win a contended exclusive resource** under competition (and not double-book it across a partition), and/or **drive a task's lifecycle** on top of a held resource. Two complementary layers:

| Layer | Answers | SDK home |
|---|---|---|
| **Gang-claim scheduler** | *which of N contending jobs atomically wins a contended island, right now, without double-booking across a partition* | the **mesh / node** surface (peer-aware, live folds) |
| **Task lifecycle** (workflow) | *what happens after it is held* — task state, the step cursor, retries, fan-out, dependencies | the **cortex** surface (a local RedEX chain) |

This is **not** capability routing (`capabilities.md`). `find_nodes` answers "who *can* do X" — an advisory read with no exclusivity. The gang scheduler answers "who *gets* X" — a contended commit where exactly one winner is guaranteed. Use `capabilities.md` to discover candidates; use this to arbitrate the contended grab.

It is also **not** a workflow engine. There is no DAG DSL, no controller loop, no Airflow. Dependencies, branches, shards, and DAGs *emerge* from a handful of primitives (leases, cursors, triggers, shard groups) over RedEX.

---

## Mental model (read before generating code)

1. **An island is a co-located pool of exclusive units.** `island_id = hash(host, domain)`, and the same `u64` *is* the reservation key. The scheduler is **resource-agnostic**: a GPU NVLink domain is the motivating instance, but a unit is any fungible exclusive sub-resource (an accelerator slot, a licensed seat, a rack PDU port). **GPU specifics ride plain capability tags** (`gpu:h100`, `model:<hex>`) — there is no GPU vocabulary in the API. (Renamed June 2026: `gpus`→`units`, `warm_models`→`capabilities`, `min_gpus`→`min_units`, `match_gpu_islands`→`match_islands`, `claim_gpu_island`→`claim_island`. Any older `*Gpu*` name is stale.)

2. **Match narrows, CAS commits.** `match_islands` is a pure, read-only narrowing over two folds (capability + live island topology). It returns a *claim-order candidate list* — advisory, safe to re-run on a reject. It holds nothing. The actual exclusive grab is a **single CAS against the reservation fold** (`reserve_island` / `claim_island`). Never treat a match result as a hold.

3. **Reserve is AP-optimistic; the `Active` commit is the one CP edge.** `reserve_island` is each node's local optimistic CAS (this node's view) plus a broadcast — fast, available, may race. Transitioning a reservation to `Active` (start irreversible work) is the only strongly-consistent step: it requires a **quorum** of the island's replica set and a **fencing epoch**, so the minority side of a partition can never start compute and a stale ex-leader is fenced out. That guarantee lives in the core; the SDK surface exposes the AP reserve/claim. A single-island `claim_island` is one CAS — atomic and deadlock-free because the island *is* the resource id.

4. **A task is a single-writer RedEX chain.** The lifecycle layer is a deterministic fold of transition events into a `TaskState { step, status, attempts }`. The single writer is the lease holder; reopening the chain replays history exactly (failover-resume). Semantics emerge — `after_task` is a dependency edge, a shard group is a set of ids plus a join predicate, a capability-bearing step routes through the gang seam.

If the user says "the scheduler queues my job and a controller assigns it" — stop. There is no central queue or controller. Each contender runs the same local match→CAS; arbitration falls out of the reservation chain's total order.

---

## The gang surface (publish → match → reserve/claim)

Five node methods. They hang off a live, connected mesh node (like `announce_capabilities`), and read this node's **local** folds — a node sees its own published islands immediately; peer-hosted islands appear only after their announcements converge.

| Method | Shape | What it does |
|---|---|---|
| `publish_island_topology(record)` | async → peer count | announce an island this node hosts (host forced to self); self-indexes + broadcasts. Re-publish each heartbeat to refresh the live axes. |
| `match_islands(criteria)` | sync, read-only → `[island_id]` | narrow capability + numeric filters → claim-ordered candidates. Best first. Empty = nothing matched. |
| `reserve_island(island, until_unix_us)` | async → `"won"` / `"lost"` | optimistic AP CAS until the takeover deadline. |
| `release_island(island)` | async → `"won"` / `"lost"` | release a held island (`"lost"` if not the holder). |
| `claim_island(criteria, until_unix_us)` | async → `island_id?` | match + reserve the first available in one call (walks candidates on `Reserved` rejects). |

### Match criteria — two filter axes

The criteria carry a **host capability match** (step 1, over the capability fold) and an **island resident/numeric match** (step 2, over the live topology fold). Both are reachable from every flat SDK surface:

| Field | Axis | Semantics |
|---|---|---|
| `tags_all` | host | host must carry **all** these tags (AND) |
| `tags_any` | host | host must carry **≥1** (OR); empty = no constraint |
| `tag_groups_all` | host | **≥1 per group** (AND of ORs); empty = no constraint |
| `min_units` | island | island has at least this many exclusive units |
| `max_load` | island | live load ≤ this (`0.0..=1.0`) |
| `max_p50_latency_us` | island | live p50 latency ≤ this |
| `require_all` | island | resident capabilities the island must have **all** of (AND) |
| `require_any` | island | resident capabilities the island must have **≥1** of (OR); empty = no constraint |
| `selection` | order | `least_loaded` (default/spread) / `pack` / `load_band` / `lowest_id` |
| `load_band_target` | order | target load for `load_band` |
| `prefer_capability` | order | **soft** affinity — islands with this capability resident rank ahead (e.g. a warm model that skips cold-load), within the selection policy |

`tags_*` filter *which hosts* are candidates; `require_*` filter *which of their islands* qualify by resident capability. `prefer_capability` is a preference, not a hard filter — cold islands still considered, just after warm ones. An empty `require_any` / `tags_any` is **no constraint** (composite-filter semantics), never "matches nothing".

The island record a node publishes: `{ id, units: [u32], capabilities: [tag], load, p50_latency_us }` — `capabilities` are resident tags (e.g. `"model:<hex>"` for a warm model). `host` is forced to the publishing node.

### Per-SDK gang API

| SDK | Match / claim | Criteria type | Record type |
|---|---|---|---|
| Rust (`net-sdk`) | `mesh.match_islands(&c)` / `mesh.claim_island(&c, until).await` | `gang::MatchCriteria` (build directly — full `CapabilityQuery`) | `gang::IslandRecord { id, units: UnitSet, host, capabilities, load, p50_latency_us }` |
| TypeScript | `node.matchIslands(c)` / `await node.claimIsland(c, until)` | `IslandCriteria` (object) | `IslandTopologyInput` |
| Python | `mesh.match_islands(tags_all, *, min_units=…, require_all=…, …)` | flat kwargs | `mesh.publish_island_topology(id, units, capabilities, load, p50)` |
| Go | `node.MatchIslands(crit)` / `node.ClaimIsland(crit, until)` | `GangCriteria{ TagsAll, TagsAny, MinUnits, RequireAll, RequireAny, … }` | `IslandRecord{ ID, Units, Capabilities, Load, P50LatencyUs }` |
| C | `net_mesh_match_islands(h, criteria_json, …)` / `net_mesh_claim_island(…)` | JSON string | JSON string |

**Rust** (build `MatchCriteria` directly — gets the full host `CapabilityQuery`):

```rust
use net_sdk::gang::{MatchCriteria, NumericFilter, SelectionPolicy};
use net_sdk::gang::{CapabilityFilter, CapabilityQuery, IslandRecord, UnitSet};

mesh.publish_island_topology(IslandRecord {
    id: 0xD0,
    units: UnitSet::new(vec![0, 1, 2, 3, 4, 5, 6, 7]),
    host: 0,                              // overwritten with this node's id
    capabilities: vec!["model:7b".into()],
    load: 0.1,
    p50_latency_us: 800,
}).await?;

let crit = MatchCriteria {
    capability: CapabilityQuery::Composite(CapabilityFilter {
        tags_all: vec!["gpu:h100".into()],   // host must carry this
        ..Default::default()
    }),
    numeric: NumericFilter {
        min_units: 8,
        require_any: vec!["model:7b".into(), "model:13b".into()], // resident ≥1
        ..Default::default()
    },
    selection: SelectionPolicy::LeastLoaded,
    prefer_capability: Some("model:7b".into()),   // soft warm affinity
};

let until = now_micros() + 60_000_000;
if let Some(island) = mesh.claim_island(&crit, until).await? {
    // exclusive hold on `island` until `until` (or release). Run the job,
    // then mesh.release_island(island).await?;
}
```

**Python** (flat kwargs):

```python
mesh.publish_island_topology(0xD0, [0,1,2,3,4,5,6,7], ["model:7b"], 0.1, 800)
ids = mesh.match_islands(["gpu:h100"], min_units=8,
                         require_any=["model:7b", "model:13b"],
                         prefer_capability="model:7b", selection="least_loaded")
island = mesh.claim_island(["gpu:h100"], UNTIL, min_units=8)   # or None
```

**Go**:

```go
node.PublishIslandTopology(net.IslandRecord{
    ID: 0xD0, Units: []uint32{0,1,2,3,4,5,6,7},
    Capabilities: []string{"model:7b"}, Load: 0.1, P50LatencyUs: 800,
})
crit := net.GangCriteria{TagsAll: []string{"gpu:h100"}, MinUnits: 8,
    RequireAny: []string{"model:7b"}, Selection: "least_loaded"}
island, found, _ := node.ClaimIsland(crit, until)   // found=false → nothing/contended
```

---

## Task lifecycle (the workflow layer) — what runs *after* you hold an island

`WorkflowAdapter` is a typed wrapper over a single-writer RedEX chain. Open it on a `Redex`, then drive transitions; the state is the deterministic fold. Distinct from the cortex **tasks** model (`cortex.md`): the workflow `TaskStatus` is a lifecycle machine — `Submitted → Running → Waiting / Blocked → Done / Failed` (`Done`/`Failed` terminal). Lives under `cortex::workflow::` to avoid name collision with the tasks model.

**State machine** — each transition returns an append seq (`wait_for_seq(seq)` to observe the fold catch up):

- `submit(id)` → `Submitted` (step 0). `start` → `Running`. `wait` → `Waiting` (parked on a trigger/claim). `block` → `Blocked` (parked on external state, recoverable). `complete` → `Done`. `fail` → `Failed`.
- `advance(id)` — step cursor +1, resets attempts. `retry(id)` — attempts +1, back to `Running`. Terminal tasks are immutable (a `Done`/`Failed` task is never moved by a stray transition; the way out of `Failed` is `retry`).
- `delete(id)` — cascades over the linked subtree (shards / spawned children) and reclaims it. `link(parent, child)` records lineage so delete cascades. `request_cancel(id)` — a worker-observed cancel signal.
- Reads: `get(id) -> TaskState?`, `status_counts()`, `subtree(id)`. Durability: `snapshot()` / `open_from_snapshot()` bound failover replay.

**Shards (fan-out / fan-in)** — map-reduce as emergence. `ShardGroup` (derive ids via `derive_shard_ids`), `fan_out` submits them (each an ordinary task with its own lease), `try_join` / `try_join_with(JoinPolicy)` submits the reduce once they finish. A **failed shard never hangs the reduce** — it surfaces as `Join::Failed`; `propagate_failure` (cancel siblings + fail parent) or `block_on_failure` (park parent `Blocked`, recoverable) applies the disposition. `JoinPolicy`: `AllOrNothing` (default) / `BestEffort` / `Threshold(n)`.

**Triggers (dependencies / branches / DAGs)** — `TriggerEngine` is a pure, indexed predicate evaluator. `arm(trigger, action)`, then drive with `on_task_change(id)` / `on_tick(now)` / `on_delete(id)`; it returns the satisfied `Action`s for *the caller* to apply (it starts nothing itself — that's what keeps it a substrate, not a controller loop). `Trigger`: `AfterTask(id)` (on `Done`), `AfterTerminal(id)` (on `Done` *or* `Failed` — failure-propagation primitive), `IfResult{task,key,value}` (branch), `AtTick(n)` (logical clock). `Action`: `Submit(id)` / `Start(id)`. Determinism is preserved — the clock is an explicit `tick`, never `now()`.

**Rust:**

```rust
use net_sdk::cortex::{workflow::WorkflowAdapter, Redex};

let wf = WorkflowAdapter::open(&redex, 0xABCD_EF01).await?;
wf.submit(1)?;                 // Submitted
wf.start(1)?;                  // Running
let seq = wf.complete(1)?;     // Done (terminal)
wf.wait_for_seq(seq).await.ok();
assert!(wf.get(1).unwrap().status.is_terminal());
```

### Per-SDK workflow API

| SDK | Open | Transitions |
|---|---|---|
| Rust | `WorkflowAdapter::open(&redex, origin).await` | `wf.submit/start/wait/block/complete/fail/advance/retry/delete/link/request_cancel` |
| TypeScript | `new WorkflowAdapter(...)` (`net_sdk` cortex) | `wf.submit/start/…` |
| Python | `WorkflowAdapter(...)` (pyo3) | `wf.submit/start/…` |
| Go | `OpenWorkflow(redex, originHash, persistent)` | `wf.Submit/Start/Wait/Block/Complete/Fail/Advance/Retry/Delete/RequestCancel/Link` |
| C | `net_workflow_adapter_open(...)` | `net_workflow_submit/start/wait/block/complete/fail/advance/retry/delete(...)` |

### The capability-bearing step (the one cross-layer seam)

A step that needs an *exclusive* capability must obtain it through the gang match→claim pipeline and **must not run** until an `Active` claim is held. In Rust this is `drive_capability_step(wf, pipeline, task, req)` (`net_sdk::gang` / core `cortex::workflow::step`): the function holds no fold, so a step *cannot* bypass the scheduler by construction. On reject the task parks `Waiting`; a minority-partition leader is quorum-starved and never starts compute. The matching `release_step` returns the island to the pool on any abnormal exit (failed / cancelled / deleted / rewound) — an un-released claim is a stranded resource.

---

## Gotchas

- **A match is advisory; the CAS is the arbiter.** Two nodes can both match the same island. Exactly one wins the `reserve`/`claim` CAS; the loser gets `"lost"` / walks to the next candidate. Don't gate exclusivity on the match.
- **`claim_island` is single-island.** It reserves the first available candidate. Multi-island *gang* acquisition (all-or-none across several islands, deadlock-free via ascending-id ordered-acquire) is a core primitive not yet on the flat SDK; build over `reserve_island` in id order, or ask for the gang surface.
- **Reserve is per-node-optimistic.** `"won"` means "won in *this node's* view" + broadcast. Cross-node exclusivity converges through the reservation fold; the strong guarantee is the quorum `Active` commit, which the flat SDK does not yet drive on-wire (the logic is tested in-process).
- **Re-publish to stay live.** Island `load` / `p50_latency_us` are live axes on a 30s TTL. A host that stops heartbeating drops out of the topology; re-publish each heartbeat.
- **Don't reach for `find_best_node`.** That's capability *placement* (advisory, `capabilities.md`). For a contended exclusive grab, `match_islands` + `claim_island`.
- **Terminal is terminal.** A `Done`/`Failed` task is immutable; `retry` is the only `Failed → Running` exit, and a fresh `submit` of the same id resets it.
- **The trigger engine applies nothing itself.** It returns `Action`s; the caller submits/starts. If "nothing fires," check you're calling `on_task_change` after the transition and applying the returned actions.

## Source of truth

- Core: `net/crates/net/src/adapter/net/behavior/gang/` (scheduler) + `behavior/fold/island.rs` (topology fold) + `cortex/workflow/` (lifecycle).
- SDK: Rust `sdk/src/gang.rs` + `sdk/src/cortex/workflow.rs`; TS `sdk-ts/src/mesh.ts` + `cortex.ts`; Python `sdk-py/src/net_sdk/mesh.py`; Go `go/mesh.go` + `go/cortex.go`; C `include/net.go.h` + `include/net_cortex.h`.
