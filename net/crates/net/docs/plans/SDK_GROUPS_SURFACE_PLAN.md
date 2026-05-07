# SDK Groups Surface Plan

Bring `ReplicaGroup` / `ForkGroup` / `StandbyGroup` — the three HA/scaling overlays already implemented in `adapter::net::compute` — into the `net-sdk` Rust surface and through to the Node, Python, and Go bindings. Mirrors the shape of `SDK_COMPUTE_SURFACE_PLAN.md`.

Groups sit **above** `DaemonRuntime`: they spawn and coordinate multiple daemons through the runtime's registry, hand out routing/placement decisions, and drive recovery on node failure. Without SDK wrappers, callers today have to reach into `adapter::net::compute::*` directly — same hazard the compute plan originally closed for `DaemonRuntime`.

## Goals

- Three SDK types — `ReplicaGroup`, `ForkGroup`, `StandbyGroup` — each taking a `&DaemonRuntime` and a `DaemonFactory`-shaped closure, not raw `Scheduler` / `DaemonRegistry` references.
- Same feature gating as compute (`compute` on `net-sdk`; adds a `groups` sub-feature that implies `compute`).
- Parity across Node (NAPI), Python (PyO3), Go (CGO). Same behavior, same structured errors, same factory-callback infrastructure — reuse the trampolines we already built for migration-target reconstruction.
- Typed `GroupError` surface on every language, with kind discrimination for `no-healthy-member` / `placement-failed` / etc. Mirrors the `MigrationError` pattern.

## Non-goals

- **Changing core semantics.** The core `ReplicaGroup` / `ForkGroup` / `StandbyGroup` stay unchanged. SDK code is wrap-and-forward, not a rewrite.
- **Automatic failure detection.** `on_node_failure` / `on_node_recovery` stay caller-driven (the caller plugs in whatever health-check loop fits their app). SDK doesn't start a background supervisor; that's a Stage N+1 decision once the basic surface is in use.
- **Group-level migration.** Cross-node migration of a whole group (vs. migration of a single daemon) is out of scope — individual group members can still be migrated via the existing `DaemonRuntime.startMigration`, but "migrate the group" is a future primitive.
- **Public release wheels carrying compute + groups.** The current Python / Node release workflows build with just `redis`. Shipping groups publicly is a separate decision (same as the documented compute-in-release gap).

---

## The three abstractions (context)

### `ReplicaGroup`

N interchangeable copies of the same daemon. All replicas share a deterministic keypair derived from `group_seed + index`, so a failed replica on node X can be respawned on node Y with the **same identity** — callers routing by origin_hash don't care which node handles the request. Placement is capability-based-spread (`scheduler.place_with_spread`). Routing is load-balanced across healthy members.

**Use case:** horizontal scaling of stateless (or eventually-consistent) daemon work — LLM inference workers, embedding servers, anything where "give me any one of N" is the routing contract.

### `ForkGroup`

N independent daemons forked from a common parent at a specific `fork_seq`. Unlike replicas, each fork has a **unique** identity + its own causal chain, but carries a `ForkRecord` that cryptographically documents its ancestry. Forks don't share state after the split.

**Use case:** branching evaluation — speculative execution, A/B exploration, MCTS-style tree expansion where N workers independently extend a shared starting state. Lineage verification lets a supervisor verify that all fork outputs descend from the right parent.

### `StandbyGroup`

Active-passive replication. One member processes events; N−1 standbys hold snapshots and catch up lazily via `sync_standbys(...)`. On active failure, `promote(...)` picks the standby with the highest `synced_through` sequence and makes it active, replaying any events that arrived after the last sync.

**Use case:** single-writer-with-failover state. Think durable counter, ledger, coordinator — one active owns the write-lock-equivalent; standbys keep warm copies for failover but consume near-zero compute in steady state.

---

## SDK design decisions

### 1. Hide `Scheduler` + `DaemonRegistry` behind `DaemonRuntime`

The core group constructors take `&Scheduler, &DaemonRegistry` directly. Exposing those raw types in the SDK surface would leak core internals across three language bindings — and would require matching FFI wrappers for `Scheduler` / `DaemonRegistry` that the compute surface deliberately avoided.

**Decision:** add `pub(crate)` accessors on the SDK's `DaemonRuntime`:

```rust
impl DaemonRuntime {
    pub(crate) fn scheduler(&self) -> &Scheduler { &self.inner.scheduler }
    pub(crate) fn registry(&self) -> &DaemonRegistry { &self.inner.registry }
}
```

The SDK group types call these under the hood. Cross-binding callers never see `Scheduler` / `DaemonRegistry`; they pass a `&DaemonRuntime`.

### 2. Reuse the migration-target factory-callback infrastructure

Groups call the daemon factory every time they spawn a member — at construction, during `scale_to`, and on `on_node_failure` replacement. The closure type is the same `Fn() -> Box<dyn MeshDaemon>` we already wire for migration-target reconstruction.

**Decision:** reuse the existing factory trampolines we built for compute:
- **NAPI**: `FactoryTsfn` / `DaemonBridgeTsfns` from `bindings/node/src/compute.rs`. Users register a group-capable factory via `runtime.registerFactory(kind, () => new MyDaemon())` (already exists), and the group wrapper resolves `kind → factory` through the same SDK factory map.
- **PyO3**: `factoryFuncs` map + `Python::attach` dispatch — same closure type already set up in `bindings/python/src/compute.rs`.
- **Go**: `factoryFuncs` map + `goComputeFactory` trampoline from `bindings/go/compute-ffi/`. `RegisterFactoryFunc(kind, factory)` is already the entry point.

This means **no new dispatcher callbacks are needed**. Groups just need the ability to look up a registered factory by `kind` and hand the resulting closure to the core group constructor.

### 3. Interior mutability so SDK method signatures stay `&self`

Core group methods take `&mut self` (`scale_to`, `on_node_failure`, `on_node_recovery`, `sync_standbys`, `promote`, `on_event_delivered`). Exposing `&mut self` to async callers forces awkward borrow juggling and doesn't match the `&self` pattern of `DaemonRuntime`.

**Decision:** the SDK wrapper holds `Arc<Mutex<CoreReplicaGroup>>` and exposes `&self` methods that lock briefly. Group operations are infrequent (seconds-to-minutes scale — nobody scales a replica group 1000 times a second), so the lock contention is non-issue. Matches how `DaemonRuntime` handles its own internal state today.

---

## Stage 0 — `net-sdk` scaffolding

Add a `groups` sub-feature that depends on `compute`. Everything below sits behind it.

```toml
[features]
compute = ["net"]
groups = ["compute"]
```

Module layout:

```
sdk/src/groups/
├── mod.rs         — pub use of the three group types + shared types
├── replica.rs     — ReplicaGroup wrapper
├── fork.rs        — ForkGroup wrapper
├── standby.rs     — StandbyGroup wrapper
├── error.rs       — GroupError (wraps core GroupError + adds SDK-level variants)
└── common.rs      — GroupHealth, MemberInfo (re-exported from core), GroupConfig helpers
```

`DaemonRuntime` gains `pub(crate)` accessors: `scheduler()`, `registry()`, `factory_for_kind(kind)`. Already partly exists via `factory_for_kind`; surface the others.

---

## Stage 1 — Rust SDK surface

### `ReplicaGroup`

```rust
pub struct ReplicaGroup { inner: Arc<Mutex<CoreReplicaGroup>>, runtime: DaemonRuntime }

impl ReplicaGroup {
    pub async fn spawn(
        runtime: &DaemonRuntime,
        kind: &str,              // looked up via runtime.factory_for_kind
        config: ReplicaGroupConfig,
    ) -> Result<Self, GroupError>;

    pub fn route_event(&self, ctx: &RequestContext) -> Result<u32, GroupError>;
    pub async fn scale_to(&self, n: u8) -> Result<(), GroupError>;
    pub async fn on_node_failure(&self, failed_node_id: u64) -> Result<Vec<u8>, GroupError>;
    pub fn on_node_recovery(&self, recovered_node_id: u64);
    pub fn health(&self) -> GroupHealth;
    pub fn group_id(&self) -> u32;
    pub fn replicas(&self) -> Vec<MemberInfo>;
    pub fn replica_count(&self) -> u8;
    pub fn healthy_count(&self) -> u8;
}
```

Notes:
- `spawn` is `async` because it hits the SDK's scheduler/registry chain; core `spawn` is sync but we wrap it in an `async` method for parity with `DaemonRuntime.spawn`.
- `route_event` stays sync — it's a pure read of the load-balancer state.
- Factory resolution: `runtime.factory_for_kind(kind)` returns an `Arc<dyn Fn() -> Box<dyn MeshDaemon> + Send + Sync>`. Pass that to the core constructor.

### `ForkGroup`

```rust
pub struct ForkGroup { inner: Arc<Mutex<CoreForkGroup>>, runtime: DaemonRuntime }

impl ForkGroup {
    pub async fn fork(
        runtime: &DaemonRuntime,
        kind: &str,
        parent_origin: u32,
        fork_seq: u64,
        config: ForkGroupConfig,
    ) -> Result<Self, GroupError>;

    pub fn route_event(&self, ctx: &RequestContext) -> Result<u32, GroupError>;
    pub async fn scale_to(&self, n: u8) -> Result<(), GroupError>;
    pub async fn on_node_failure(&self, failed_node_id: u64) -> Result<Vec<u8>, GroupError>;
    pub fn on_node_recovery(&self, recovered_node_id: u64);
    pub fn health(&self) -> GroupHealth;
    pub fn parent_origin(&self) -> u32;
    pub fn fork_seq(&self) -> u64;
    pub fn fork_records(&self) -> Vec<ForkRecord>;  // owned clones; core returns &ForkRecord
    pub fn verify_lineage(&self) -> bool;
    pub fn members(&self) -> Vec<MemberInfo>;
    pub fn fork_count(&self) -> u8;
    pub fn healthy_count(&self) -> u8;
}
```

Notes:
- `fork_records()` returns owned clones so the Mutex guard drops before the caller uses the data. Acceptable since `ForkRecord` is small (a few fields + sentinel bytes).

### `StandbyGroup`

```rust
pub struct StandbyGroup { inner: Arc<Mutex<CoreStandbyGroup>>, runtime: DaemonRuntime }

impl StandbyGroup {
    pub async fn spawn(
        runtime: &DaemonRuntime,
        kind: &str,
        config: StandbyGroupConfig,
    ) -> Result<Self, GroupError>;

    pub fn active_origin(&self) -> u32;
    pub fn on_event_delivered(&self, event: CausalEvent);
    pub async fn sync_standbys(&self) -> Result<u64, GroupError>;
    pub async fn promote(&self) -> Result<u32, GroupError>;
    pub async fn on_node_failure(&self, failed_node_id: u64) -> Result<Option<u32>, GroupError>;
    pub fn on_node_recovery(&self, recovered_node_id: u64);
    pub fn health(&self) -> GroupHealth;
    pub fn active_healthy(&self) -> bool;
    pub fn active_index(&self) -> u8;
    pub fn member_role(&self, index: u8) -> Option<MemberRole>;
    pub fn synced_through(&self, index: u8) -> Option<u64>;
    pub fn buffered_event_count(&self) -> usize;
    pub fn group_id(&self) -> u32;
    pub fn members(&self) -> Vec<MemberInfo>;
    pub fn member_count(&self) -> u8;
    pub fn standby_count(&self) -> u8;
}
```

Open question: should `DaemonRuntime.deliver` transparently invoke `standby_group.on_event_delivered` for events routed to an active? Two options:
- **(a) Manual**: caller must `group.on_event_delivered(event)` after every `runtime.deliver(active_origin, event)`. Explicit but error-prone.
- **(b) Registration hook**: `StandbyGroup::spawn` registers a post-deliver hook on the runtime's active origin; `runtime.deliver` fans out to the hook automatically. Invisible integration, but adds a hook mechanism to `DaemonRuntime`.

Recommendation: **(a) for Stage 1**, revisit (b) when a second use case for post-deliver hooks emerges. Manual is no worse than what core callers do today.

### `GroupError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum GroupError {
    #[error("no healthy member available")]
    NoHealthyMember,
    #[error("placement failed: {0}")]
    PlacementFailed(String),
    #[error("registry failed: {0}")]
    RegistryFailed(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("factory not found for kind '{0}'")]
    FactoryNotFound(String),
    #[error("runtime not ready")]
    NotReady,
    #[error(transparent)]
    Core(#[from] CoreGroupError),
}
```

Wraps the core `GroupError` via `#[from]` so existing core errors flow through without restructuring.

### Exit criteria (Stage 1)

- Rust SDK integration test: spawn a `ReplicaGroup` of 3 with a fake scheduler, route 100 events, assert all 3 members received roughly a third.
- Rust SDK test: `ForkGroup::fork` from a parent daemon, `verify_lineage()` returns true, each fork has a unique `origin_hash`.
- Rust SDK test: `StandbyGroup` with 3 members, deliver 10 events to active, `sync_standbys()`, fail the active, `promote()` picks the right standby with `synced_through=10`.
- Clean `cargo test -p net-sdk --features groups`.

---

## Stage 2 — TypeScript (NAPI)

### NAPI crate additions

New module `bindings/node/src/groups.rs` behind `feature = "compute"` (no separate `groups` feature — if you want compute, you get groups).

Three NAPI classes: `ReplicaGroup`, `ForkGroup`, `StandbyGroup`. Each:
- `#[napi(factory)]` method builds the group from `&DaemonRuntime`.
- All `&mut self` core methods become `pub async fn` returning `Promise<T>` via `env.spawn_future`.
- All sync-read core methods stay sync on the NAPI side.

### TS SDK additions

`sdk-ts/src/groups.ts`. Exports three classes matching the NAPI shape:

```typescript
export class ReplicaGroup {
  static async spawn(rt: DaemonRuntime, kind: string, config: ReplicaGroupConfig): Promise<ReplicaGroup>;
  routeEvent(ctx: RequestContext): Promise<number>;
  scaleTo(n: number): Promise<void>;
  onNodeFailure(failedNodeId: bigint): Promise<Uint8Array>;
  onNodeRecovery(recoveredNodeId: bigint): void;
  health(): GroupHealth;
  groupId(): number;
  replicas(): MemberInfo[];
  replicaCount(): number;
  healthyCount(): number;
}
```

Plus `ForkGroup` and `StandbyGroup` with the analogous shapes. `GroupError` as a typed exception class with `kind` discriminator — parser reads the `daemon: group: <kind>[: detail]` prefix emitted by the Rust side.

### Error-kind wire format

Extend the NAPI `daemon_err_from_sdk` to handle `SdkGroupError`:

```
daemon: group: no-healthy-member
daemon: group: placement-failed: <detail>
daemon: group: registry-failed: <detail>
daemon: group: invalid-config: <detail>
daemon: group: factory-not-found: <kind>
daemon: group: not-ready
```

TS `GroupError extends DaemonError` with `kind: GroupErrorKind` + `detail?: string`. Same pattern as `MigrationError`.

### Tests (TS)

- `replica-group.test.ts`: spawn ReplicaGroup of 3 on a single-node mesh, route 100 events, assert distribution; scaleTo(5) grows, scaleTo(2) shrinks; onNodeFailure/Recovery round-trip.
- `fork-group.test.ts`: fork from a parent daemon, verify lineage, assert unique origin_hashes.
- `standby-group.test.ts`: spawn, deliver, sync, promote — on-demand failover test.
- `groups-errors.test.ts`: FactoryNotFound, invalid-config, no-healthy-member after all members forced unhealthy.

### Exit criteria (Stage 2)

- All TS tests pass.
- `GroupError`, `GroupHealth`, `MemberInfo`, `MemberRole`, `ForkRecord`, `RequestContext` exported from `@ai2070/net-sdk`.
- Distribution test on ReplicaGroup confirms load-balanced routing.

---

## Stage 3 — Python (PyO3)

Structure mirrors Stage 2:

- `bindings/python/src/groups.rs` behind `compute` feature.
- Three `#[pyclass]` types with `#[pyo3(signature = (...))]` methods.
- Async methods use the existing `py.detach() + runtime.block_on(...)` pattern.
- Sync methods that lock a mutex briefly (e.g., `health()`, `members()`) stay sync.
- `GroupError` via `pyo3::create_exception!(_net, GroupError, DaemonError, "...")` — subclass of `DaemonError`, same parent-class discipline as `MigrationError`.
- Python-side `group_error_kind(exc)` helper parses the `daemon: group: <kind>` prefix.

### Python tests

- `test_groups.py`: ReplicaGroup spawn + route + scale; ForkGroup fork + verify_lineage; StandbyGroup promote; error-kind surface tests.

### Exit criteria (Stage 3)

- 20+ Python tests passing.
- `GroupError`, `group_error_kind`, three group classes, `GroupHealth`, `MemberInfo`, `MemberRole`, `ForkRecord` exported from `net` package.

---

## Stage 4 — Go (CGO)

Structure mirrors Stage 2+3 but with the `compute-ffi` crate as the host:

- New module `bindings/go/compute-ffi/src/groups.rs` — opaque handle types `ReplicaGroupHandle`, `ForkGroupHandle`, `StandbyGroupHandle`.
- Each group operation gets its own extern "C" fn (≈ 10–15 per type).
- Returns use the same error pattern as migration: `c_int` + `*mut *mut c_char` for structured detail.
- Factory resolution reuses the existing `net_compute_register_factory_with_func` path — groups call back into Go's `factoryFuncs` map via the dispatcher's `factory` trampoline. **No new dispatcher callbacks.**

### C header additions

`net.h` gets ~40 new declarations:

```c
typedef struct net_compute_replica_group_s  net_compute_replica_group_t;
typedef struct net_compute_fork_group_s     net_compute_fork_group_t;
typedef struct net_compute_standby_group_s  net_compute_standby_group_t;

int  net_compute_replica_group_spawn(rt, kind, kind_len, replica_count, out, err_out);
void net_compute_replica_group_free(g);
int  net_compute_replica_group_scale_to(g, n, err_out);
/* ... */
```

### Go package additions

`bindings/go/net/groups.go` — 3 types, each with a runtime.SetFinalizer + sync.RWMutex wrapper. `*GroupError` type with `Kind MigrationErrorKind`-style discriminator (reuse the parse helper pattern from `migration.go`, parameterize on the `group:` vs `migration:` prefix).

### Go tests

`groups_test.go`:

- ReplicaGroup spawn/scale/route/distribution.
- ForkGroup spawn + lineage-verifiability + unique origin_hashes.
- StandbyGroup spawn + sync + promote, including the end-to-end failover assertion (active fails → standby promoted → subsequent event lands on new active).

### Exit criteria (Stage 4)

- `go test ./...` clean on `bindings/go/net`.
- End-to-end failover test proves a StandbyGroup survives an active-node failure without external coordination.

---

## Critical files

### Stage 0–1 (Rust)
- `net/crates/net/sdk/Cargo.toml` — add `groups` feature.
- `net/crates/net/sdk/src/lib.rs` — wire `pub mod groups` under `feature = "groups"`.
- `net/crates/net/sdk/src/compute.rs` — add `pub(crate) fn scheduler()` / `registry()` / expose `factory_for_kind` cross-module.
- `net/crates/net/sdk/src/groups/` (new directory) — the module.

### Stage 2 (TS)
- `net/crates/net/bindings/node/src/groups.rs` (new) — three `#[napi]` classes.
- `net/crates/net/bindings/node/src/compute.rs` — extend `daemon_err_from_sdk` with the `group:` prefix path.
- `net/crates/net/sdk-ts/src/groups.ts` (new) — TS classes.
- `net/crates/net/sdk-ts/src/errors.ts` (if present; otherwise `compute.ts`) — `GroupError`.

### Stage 3 (Python)
- `net/crates/net/bindings/python/src/groups.rs` (new).
- `net/crates/net/bindings/python/src/compute.rs` — extend `daemon_err_from_sdk` with the `group:` path + `group_error_kind` helper in `__init__.py`.

### Stage 4 (Go)
- `net/crates/net/bindings/go/compute-ffi/src/groups.rs` (new).
- `net/crates/net/bindings/go/net/net.h` — group declarations.
- `net/crates/net/bindings/go/net/groups.go` (new).

### CI updates
- `.github/workflows/ci.yml` — add `groups` to the feature sets for Python + Node + (when Go CI lands) Go. Same pattern as the recent `compute` addition.

---

## Open questions

1. **Deliver-hook for StandbyGroup.** Per the design-decision section, defaulting to manual `on_event_delivered` calls. Worth revisiting if users consistently forget to call it and standbys fall behind.

2. **Group persistence across process restarts.** Core groups are in-memory — if the process dies, group membership evaporates. A `GroupRegistry` that survives restart (via redex-disk?) is a follow-on feature; out of scope here but worth flagging.

3. **Cross-group interactions.** Can a daemon be a member of both a `ReplicaGroup` and a `StandbyGroup`? Core currently lets you register the same `origin_hash` once in the `DaemonRegistry`, so the answer is no at the registry layer — but we should surface a clear error in the SDK if a caller tries.

4. **Request context plumbing.** Core `route_event` takes a `RequestContext`. The TS / Python / Go surface needs to construct one of these on the caller's side; the simplest shape is a POJO with an optional sticky-key for session affinity. Spec the exact shape during Stage 1 design once we look at `core::RequestContext` fields.

5. **Factory vs. explicit-instance spawn.** For `DaemonRuntime.Spawn`, we support both (`spawn` takes an instance; `spawn_with_daemon` is internal). Should groups? The argument for instance-based is type-symmetry with spawn; the argument for kind-based is that groups spawn N members, so "bring your own instance" only works for the first one. **Recommendation: kind-based only.** Groups need a factory by definition.

6. **Shipping in release workflows.** Not blocking Stage 1–4, but open: eventually we'll want the publish workflows to build with `groups` + `compute` + `net` for at least one Linux target so downstream users can `pip install net` / `npm install @ai2070/net` and get the group surface. Right now only `redis` ships.

---

## Rough estimates

- **Stage 0–1 (Rust SDK)**: 1.5–2 days. Thin wrapping, no new machinery.
- **Stage 2 (TS)**: 2 days. Three NAPI classes × ~10 methods each + TS wrappers + tests. No new dispatcher plumbing.
- **Stage 3 (Python)**: 1.5 days. PyO3 parallels are tight; similar scope to TS.
- **Stage 4 (Go)**: 2.5 days. More ceremony (C header + .c bridge file + Go wrappers + CGO finalizer discipline) but again, no new dispatcher callbacks.
- **Total**: ~1.5 weeks of focused work for all four languages, comparable in scope to Stages 3–6 of the compute plan.
