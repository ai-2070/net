# Aggregator lifecycle — deferred items (2026-05-23)

Branch: `subnet-scaling`.
Tip after this slice: `09492dc5` ("feat(aggregator): AggregatorGroup — N replicas managed via LifecycleHandle").
Scope: Phase B slice 4 of `docs/plans/SCALING_SUBNET_SPEC.md` — `AggregatorDaemon` lifecycle + group management.

Slice 4 shipped:

- `LifecycleDaemon` async sibling trait + `LifecycleHandle` RAII wrapper (`19faf1e3`).
- `AggregatorDaemon` impls `LifecycleDaemon` (background tokio loop, cooperative shutdown).
- `AggregatorGroup` — N replicas managed as a unit via `LifecycleHandle`s, deterministic identity via `derive_replica_keypair` (`09492dc5`).

The items below were explicitly scoped out of the slice. They are the gap between **"single-process aggregator lifetimes"** (what shipped) and **"distributed aggregator deployment"** (what the spec calls for).

Tagged `[B | H | M | L]`:

- B — blocker for cross-node aggregator deployments.
- H — closes a spec-promised capability missing from the substrate.
- M — operator footgun or premature edge worth scheduling before scale.
- L — hygiene / cleanup.

---

## Status

| ID    | Pri | Area                  | Title                                                                                 |
|-------|-----|-----------------------|---------------------------------------------------------------------------------------|
| AL-1  | H   | trait surface         | `LifecycleDaemon` sibling vs `MeshDaemon` — ✅ direction B (sibling + generalized to `LifecycleGroup<L>`, `e672793a`) |
| AL-2  | B   | placement             | `AggregatorGroup` had no cross-node placement — ✅ `spawn_with_placement` via `Scheduler`, `5acf1f34`                  |
| AL-3  | H   | failure recovery      | No per-replica health or auto-replacement — ✅ `health()` + `replace` + `HealthMonitor`, `8787b2db` / `40a1b375`        |
| AL-4  | M   | observability         | `AggregatorGroup` skipped registry — ✅ `AggregatorRegistry` on `MeshNode`, `e50728f0`                                  |
| AL-5  | M   | shutdown determinism  | `on_stop` could drop mid-publish work — ✅ shutdown-aware tick loop + bumped backstop, `4016528a` |
| AL-6  | M   | operator CLI          | `net aggregator spawn / ls / scale` — ✅ `ls` live, `spawn`/`scale` parse-only pending daemon, `cb74cc14`                |
| AL-7  | L   | dead-code warning     | `DispatchCtx.reservation_fold` field wired but unread — ✅ `cc4aac82`                  |
| AL-8  | L   | summary rendering     | Reservation summarizer's `Reserved { ... }` Debug bucket — ✅ fixed-label match arms + tighter test, `4016528a` |

---

## HIGH — close spec-promised capability

### AL-1 — `LifecycleDaemon` sibling vs spec-promised `MeshDaemon` aggregator

`docs/plans/SCALING_SUBNET_SPEC.md:5` and `:118` describe aggregators as "deployed via `ReplicaGroup` of `MeshDaemon`". We deviated:

- `MeshDaemon` (in `adapter::net::compute::daemon`) is documented sync-only / WASM-compatible: `process(&CausalEvent) -> Vec<Bytes>`.
- The aggregator role is inherently async (`tokio::interval`, `mesh.publish().await`).
- Slice 4 introduced `LifecycleDaemon` as a sibling trait rather than retrofitting async/lifecycle onto `MeshDaemon`.

This is a one-way door for the spec text. Two paths forward:

1. **Adapter shim** — write `MeshDaemonAdapter<L: LifecycleDaemon>` that implements `MeshDaemon` against a no-op `process` and drives the lifecycle on `Drop` / construction. Lets `ReplicaGroup::spawn` accept an aggregator. Friction: `MeshDaemon::requirements()` would need to be either trivial or wired through `AggregatorConfig`.
2. **Spec amendment** — update `SCALING_SUBNET_SPEC.md` to describe the sibling-trait shape and ship a parallel `LifecycleReplicaGroup` primitive (closer to what `AggregatorGroup` already is, but generic).

Recommendation: (2) once AL-2/AL-3 land. The MeshDaemon adapter would re-introduce the sync/async friction we just sidestepped.

### AL-3 — No auto-replacement or per-replica health on `AggregatorGroup`

`ReplicaGroup` carries `GroupCoordinator` with `MemberInfo { healthy, ... }` and a route_event LB. `AggregatorGroup` has neither. Failures of a replica's background loop are silently logged (`tracing::warn!`) and the loop continues without re-spawn.

**Minimum-viable**: per-replica liveness based on "last generation advanced within 3 × interval". An idle/stuck loop flips to unhealthy, group-level health goes from `all` to `degraded`, operator inspects via CLI / Deck.

**Full**: auto-respawn on unhealthy → replace via factory with same index → identity continuity preserved.

---

## BLOCKER for cross-node deployments

### AL-2 — `AggregatorGroup` has no cross-node placement

`ReplicaGroup::spawn` uses `Scheduler::place_with_spread(requirements, &used_nodes)` to spread replicas across failure domains. `AggregatorGroup::spawn` takes a factory closure — the caller picks which `MeshNode` each replica runs against.

For single-process tests and CLI-driven local deployments this is fine. For a real scaled deployment ("aggregators on 3 nodes in different racks") the operator needs:

- A factory that, given an index, selects a placement target.
- Or: the group itself owns placement and the factory only customizes the daemon's config.

The substrate side of this needs the aggregator to advertise placement requirements (which is `MeshDaemon::requirements()` shaped). That puts AL-2 squarely on top of AL-1's path-1 (adapter) or path-2 (parallel primitive).

---

## MEDIUM — operator footguns

### AL-4 — `AggregatorGroup` skips `DaemonRegistry` registration

`ReplicaGroup::spawn` calls `registry.register(host)` for each replica so the daemon shows up in `MeshOS` inspection surfaces and gets tagged for placement queries. `AggregatorGroup` doesn't touch the registry — aggregators are invisible to `net daemon ls`.

This is fine for now (aggregators have their own dedicated `net aggregator inspect` path that reads `MeshNode::aggregator_*` accessors), but means there's no unified "what daemons are running on this mesh?" view that includes aggregators.

### AL-5 — `on_stop` JoinHandle timeout can drop mid-publish work

`AggregatorDaemon::on_stop` does:

```rust
let deadline = self.config.summary_interval + Duration::from_millis(100);
let _ = tokio::time::timeout(deadline, h).await;
```

If `mesh.publish().await` is in flight at shutdown and takes longer than that, the `tokio::time::timeout` returns and the JoinHandle is dropped (which aborts the task). Risks:

- A summary that's been encoded but not yet published is lost (the receiver never sees it, but no fold state is corrupted — the next interval re-summarizes).
- A `tracing::warn!` line is emitted post-shutdown when the next runtime tick observes the aborted task.

Could be smarter: bound the in-flight publish by an `AbortHandle` checked against `shutdown.load(Acquire)` between summaries within a batch, instead of letting the whole task drop.

### AL-6 — `net aggregator spawn / ls / scale` gated on the above

CLI verbs the spec sketches:

- `net aggregator spawn --replica-count=N --source-subnet=… --visibility=…` — instantiates an `AggregatorGroup`.
- `net aggregator ls` — lists running groups + per-replica health.
- `net aggregator scale --group-id=X --replica-count=N` — adjust replica count.

Today the CLI surface is read-only (`inspect`, `query`). Adding `spawn` needs a runtime registry for live groups, which is AL-4. Adding `scale` needs AL-3 (per-replica health). Adding `ls` needs both.

---

## LOW — hygiene

### AL-7 — `DispatchCtx.reservation_fold` field unread

`net/crates/net/src/adapter/net/mesh.rs:511` carries `reservation_fold: Arc<Fold<ReservationFold>>` in `DispatchCtx`, populated but never read in the dispatch path. Build warning:

```
warning: field `reservation_fold` is never read
   --> src\adapter\net\mesh.rs:511:5
```

Either:

- Wire reservation-fold dispatch (the dispatch path currently only routes capability-fold envelopes).
- Drop the field until reservation dispatch is needed (cheaper; the field can come back when dispatch lands).

### AL-8 — Reservation summarizer's `Reserved { ... }` Debug bucket is verbose

`ReservationFoldSummarizer` derives bucket names from `format!("{state:?}").to_lowercase()`. `ReservationState::Reserved { holder, until_unix_us }` then renders as `reserved { holder: 162, until_unix_us: 17… }`. Buckets get unbounded cardinality (per holder × deadline).

The existing test (`reservation_fold_summarizer_buckets_by_state_label`) papers over this by `starts_with("reserved")` matching. A proper fix: change the summarizer to bucket by a `state.label()`-style discriminant, not by full Debug. Out of scope for slice 4 (would touch the summarizer trait surface).

---

## Cross-references

- Spec: `docs/plans/SCALING_SUBNET_SPEC.md` — design intent, the "deployed via ReplicaGroup" promise these items were gated on.
- Prior deferred review: `docs/misc/CODE_REVIEW_2026_05_23_MULTIFOLD_DEFERRED.md` — fold-framework cleanup landed earlier the same day.
- Slice 4 commits (subnet-scaling branch): `19faf1e3` (LifecycleDaemon + impl), `09492dc5` (AggregatorGroup).
- Direction B commits (aggregator-lifecycle branch):
  - `e672793a` — step 1: hoist + generalize `LifecycleGroup<L>`.
  - `5acf1f34` — step 2: `requirements()` + `spawn_with_placement`.
  - `e50728f0` — step 3: `AggregatorRegistry` on `MeshNode`.
  - `8787b2db` — step 4a: `ReplicaHealth` + `LifecycleGroup::replace`.
  - `40a1b375` — step 4b: `HealthMonitor` auto-respawn driver.
  - `cb74cc14` — step 5: `net aggregator ls / spawn / scale`.
  - `4016528a` — AL-5 + AL-8 fixes.
  - `71ccaebf` — step 6: registry holds `LifecycleGroup` directly + `register_with_monitor`.

## Remaining gap

All eight original items (AL-1..AL-8) are resolved in the substrate. The only outstanding work is the **daemon process** for live `spawn` / `scale`:

- `net aggregator spawn` and `net aggregator scale` parse + validate args today, then error with a "needs daemon process" message. The substrate primitives (`AggregatorRegistry`, `LifecycleGroup::spawn_with_placement`, `HealthMonitor`, `register_with_monitor`) are all in place — what remains is an `aggregator-daemon` binary that boots a `MeshNode`, installs the registry, and exposes a registry-RPC surface the CLI can call into.

That binary is its own slice; the deferred-items review intentionally stops at substrate readiness.
