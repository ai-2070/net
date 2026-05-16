# Deck Demo Harness — implementation plan

> The substrate-side primitives [`DECK_DEMO_PLAN.md`](DECK_DEMO_PLAN.md) needs in order to boot a real in-process multi-node cluster instead of synthetic fixtures. Six pieces: **Phase 0** (in-process multi-`Mesh` + multi-`MeshOsRuntime` harness) + **Phase 0.5** (bridge probes that glue the network layer into the MeshOS snapshot fold) + four missing items the deck demo can't build on top of yet — **A** daemon supervisor pattern across nodes, **B** real chain placement, **C** lifecycle coordination, **D** nRPC observer hook on `Mesh`. This plan ships those primitives; the demo plan consumes them.

## Status

Design only. Today the SDK can boot **one** `MeshOsRuntime` cleanly (`MeshOsDaemonSdk::start_with_verifier_and_migration_source`, exercised by `deck/src/runtime.rs:117` and every integration test). It can boot **two** raw `Mesh` instances peered over UDP loopback (`crates/net/sdk/examples/nrpc_echo.rs:69-91`). It cannot boot **N** `MeshOsRuntime` instances **with their state folds wired to a real `Mesh`** and a coherent lifecycle. Every prerequisite for `DECK_DEMO_PLAN.md`'s Phase 1 lives here.

## Architectural framing — MeshOS vs Mesh

**Critical to understand before reading the phases.** The substrate splits the network layer from the MeshOS state layer:

- **`Mesh` / `MeshNode`** (under `crates/net/src/adapter/net/`) is the network. UDP, handshake, capability broadcast, peer table, RTT measurement. This is what `nrpc_echo` boots.
- **`MeshOsRuntime`** (under `crates/net/src/adapter/net/behavior/meshos/`) is the state fold. Snapshot, scheduler, daemon registry, migration orchestrator, replica/chain machinery. Built without any network knowledge by `MeshOsRuntime::start_with_full_extensions` (sdk.rs:643) which takes an empty `ProbeRegistry`.
- **`MeshOsDaemonSdk`** wraps `MeshOsRuntime`. Today's `deck/src/runtime.rs:90-122` constructs one with `MeshOsConfig::default()`, registers no probes, and never binds a socket. Its `snapshot.peers` is empty unless a synthetic probe (`SampleLocalityProbe` et al.) is registered.

The two layers communicate through **probe traits** (`LocalityProbe`, `HealthProbe`, `InventoryProbe`) that the runtime polls on every tick. The samples mode fills the fold by registering synthetic probes that fabricate data. A real cluster needs **bridge probes** that read off a real `Mesh` and surface the data the runtime expects. Phase 0.5 ships those bridge probes; without them, Phase 0's harness would return handles whose snapshots stay empty.

## Frame

The SDK already has every load-bearing piece in isolation:

- `MeshOsRuntime` runs a single node's snapshot fold, scheduler, orchestrator, registry.
- `Mesh::connect_direct` opens a real UDP handshake against a known peer address.
- `Scheduler::place_with_spread` computes capability-spread placements once it can see peers in the `CapabilityIndex`.
- `ReplicaGroup` / `ForkGroup` / `StandbyGroup` (per `SDK_GROUPS_SURFACE_PLAN.md`) spawn coordinated daemon sets through a `DaemonRuntime` + scheduler.

What's missing is **glue at two levels**:
1. A harness that stands N `Mesh` + `MeshOsRuntime` pairs up, peers them by address, waits for the capability fold to converge.
2. Bridge probes that wire each `Mesh`'s peer table → that `MeshOsRuntime`'s probe registry so the fold reflects what the network sees.

The deck demo is the first consumer of both; the same primitives are useful for SDK integration tests today (multi-node groups, real migrations, real chain assignment) that are currently single-process / single-runtime workarounds.

## Why this exists

Three reasons this is a substrate-level plan, not deck-binary code:

1. **The harness is reusable.** "Boot N MeshOS runtimes in-process and run a scenario across them" is the shape every integration test that touches multi-node behavior wants. Putting it in `net_sdk::testing::cluster` (or similar) gives the deck demo + every future test the same primitive instead of growing parallel ad-hoc spawners.

2. **The supervisor / placement / lifecycle gaps are surface holes.** They're not deck-specific bugs. A library user trying to build a multi-node app today hits the same gaps — there's no idiomatic way to register a daemon factory "on every node in this group" or to spawn a chain whose members land via real placement. Closing the gaps closes them for everyone.

3. **Substrate work has its own review cadence.** Mixing "expose a new SDK helper" into a UX-focused deck demo plan obscures the substrate change. Split plans keep the SDK additions reviewable against substrate stability constraints (no semantic changes to chain placement, factory dispatch stays SemVer-stable, etc.).

## What ships

Four phases. Each is independently reviewable and lands a self-contained primitive.

### Phase 0 — Multi-`Mesh` + multi-`MeshOsRuntime` harness

**Goal.** A `ClusterHarness::new(n)` call returns N booted-and-peered nodes — each node is a `(Mesh, MeshOsDaemonSdk)` pair — within ~3 s on a developer laptop. Real UDP loopback, real handshake. The MeshOS layer's snapshot is **not yet populated** at this phase; that's Phase 0.5's job.

**API sketch (illustrative, not final).**

```rust
pub struct ClusterHarness {
    nodes: Vec<ClusterNode>,
}

pub struct ClusterNode {
    pub mesh: Arc<Mesh>,
    pub sdk: MeshOsDaemonSdk,
    pub local_addr: SocketAddr,
    pub node_id: NodeId,
    pub public_key: [u8; 32],
}

impl ClusterHarness {
    /// Spawn `n` (Mesh, MeshOsRuntime) pairs on
    /// `127.0.0.1:<ephemeral>` and peer every Mesh pair.
    /// Returns once every Mesh sees `n-1` peer sessions.
    /// Bridge probes (Phase 0.5) are wired during this call.
    pub async fn new(n: usize) -> Result<Self, ClusterError>;
    pub fn nodes(&self) -> &[ClusterNode];
    pub fn nth(&self, i: usize) -> &ClusterNode;
}
```

**Files.** `crates/net/sdk/src/testing/mod.rs` + `crates/net/sdk/src/testing/cluster.rs` (new module, gated behind a `testing` Cargo feature so it doesn't pollute the release surface). Re-exported as `net_sdk::testing::ClusterHarness`.

**Boot order.** (1) build N `Mesh` instances via `MeshBuilder::new("127.0.0.1:0", &psk)` — same path `nrpc_echo` uses. The kernel assigns an ephemeral port; read it back via `mesh.inner().local_addr()`. (2) For each ordered pair `(i, j)` with `i < j`, drive an `accept` + `connect` pair (the `nrpc_echo` pattern at examples/nrpc_echo.rs:81-87, generalized to N-way). (3) Build N `MeshOsDaemonSdk`s. (4) **Phase 0.5 wires bridge probes** between each Mesh and its sibling MeshOsRuntime. (5) Poll every Mesh's session table until `n-1` sessions are established, then poll every MeshOsRuntime's `snapshot.peers` until it reports `n-1` peers, with separate timeout budgets.

**Where the substrate currently blocks.** Nowhere at the Mesh layer — `MeshBuilder` + `accept`/`connect` already work. The `MeshOsRuntime` layer needs the bridge probes from Phase 0.5 before its snapshot reflects peer state. Mesh-only harness functionality is shippable on its own and is useful for tests that don't need the MeshOS state fold.

**Smoke test.** `tests/cluster_harness.rs`: boot 5 nodes, assert each `Mesh` sees 4 peer sessions within 3 s, drop the harness, assert clean shutdown (no leaked tokio tasks).

### Phase 0.5 — Bridge probes (Mesh → MeshOsRuntime)

**Goal.** Three small `LocalityProbe` / `HealthProbe` / `InventoryProbe` impls that read from a `Mesh` handle and surface what the `MeshOsRuntime`'s tick loop expects. Registered against each runtime's `ProbeRegistry` during `ClusterHarness::new`. Once installed, the runtime's `snapshot.peers` populates naturally each tick.

**Why this is its own phase.** It's the bridge that gives the harness's `MeshOsDaemonSdk` handles non-empty snapshots. Without it, the deck demo (and any test) gets back handles whose `snapshot.peers` stays empty forever even though the underlying `Mesh` instances are fully peered. The synthetic `SampleLocalityProbe` does the same shape today; this is its real counterpart.

**API sketch.**

```rust
/// Reads peer RTT from a `Mesh` and reports it through the
/// runtime's `LocalityProbe` interface. Cheap each tick — one
/// borrow of the mesh's peer table.
pub struct MeshLocalityProbe { mesh: Arc<Mesh> }
impl LocalityProbe for MeshLocalityProbe { /* ... */ }

pub struct MeshHealthProbe { mesh: Arc<Mesh> }
impl HealthProbe for MeshHealthProbe { /* ... */ }

pub struct MeshInventoryProbe { mesh: Arc<Mesh> }
impl InventoryProbe for MeshInventoryProbe { /* ... */ }

/// Convenience: install all three on a runtime in one call.
/// Called by `ClusterHarness::new` after the Mesh pairs handshake.
pub fn install_mesh_probes(sdk: &MeshOsDaemonSdk, mesh: Arc<Mesh>);
```

**Files.** `crates/net/sdk/src/testing/probes.rs` (same `testing` feature gate as the harness). Re-exported as `net_sdk::testing::{MeshLocalityProbe, MeshHealthProbe, MeshInventoryProbe, install_mesh_probes}`.

**Substrate work.** Likely none — the existing `Mesh` exposes peer iteration + RTT + capability sets via accessors the bridge probes call. If a needed accessor is missing (e.g. "is this peer reachable right now"), add it as a small substrate change in the same PR; the alternative is hand-rolling a peer-table mirror in the testing module which would invariably drift.

**Smoke test.** Extension to Phase 0's smoke test: after the 5-node mesh peers, poll each `MeshOsDaemonSdk`'s `snapshot.peers` and assert it reports 4 entries within 2 s (separate budget from the Mesh handshake). Validate health = `Healthy` for every entry and `rtt_ms` is `Some(_)`.

**Where this gets revisited.** A production-quality `MeshLocalityProbe` belongs in the SDK proper (not behind `testing`) once real deployments want it. For v1 it lives in `testing` because its only consumer is the harness; promoting it later is a rename, not a rewrite.

### Missing item A — Daemon supervisor pattern across nodes

**Goal.** From a single call site (the deck demo or a test), register a daemon `kind` on a chosen subset of nodes, with each node spawning its own instance through its own `DaemonRuntime`. No N-way manual repetition.

**Today's shape.** To run "one `HeartbeatDaemon` per node" today the caller writes:

```rust
for node in cluster.nodes() {
    let runtime = node.daemon_runtime();
    runtime.register_factory("heartbeat", || Box::new(HeartbeatDaemon::new()))?;
    runtime.spawn("heartbeat", config)?;
}
```

That works but each `spawn` is independent; if node 3 fails its registration the cluster ends up in a mixed state with no rollback. The supervisor primitive folds the loop + the rollback together.

**API sketch.**

```rust
impl ClusterHarness {
    /// Register `kind` on every node and `spawn` one instance
    /// per node. Rolls back partial registration on first error.
    pub async fn spawn_per_node<D, F>(
        &self,
        kind: &str,
        factory: F,
    ) -> Result<Vec<DaemonHandle>, ClusterError>
    where
        D: MeshDaemon + 'static,
        F: Fn() -> D + Send + Sync + 'static;

    /// Same, but only on the subset of nodes for which
    /// `predicate` returns true.
    pub async fn spawn_where<D, F, P>(
        &self,
        kind: &str,
        factory: F,
        predicate: P,
    ) -> Result<Vec<DaemonHandle>, ClusterError>
    where
        D: MeshDaemon + 'static,
        F: Fn() -> D + Send + Sync + 'static,
        P: Fn(&MeshOsDaemonSdk) -> bool;
}
```

**Substrate work.** None — this is pure composition on top of `DaemonRuntime::register_factory` + `spawn`. The rollback semantics are "loop in reverse, call `stop` on each already-spawned daemon, propagate the original error." `DaemonRuntime::stop` is already wired.

**Why it's separable from Phase 0.** Phase 0 hands back the raw `MeshOsDaemonSdk` handles. A caller who only wants "spawn one daemon on one specific node" doesn't need the supervisor; they get the handle and call `spawn` directly. The supervisor is the convenience for the multi-node-uniform case.

### Missing item B — Real chain placement

**Goal.** A demo (or test) that wants three `MixerDaemon`s on three different nodes invokes `ReplicaGroup::spawn` against the cluster and gets real `place_with_spread` placement. The chain's holders land in the substrate's chain machinery; `snapshot.replicas` reflects them through the normal fold path, not through synthetic `PlacementIntent` publishes.

**Today's shape.** `ReplicaGroup` already exists (per `SDK_GROUPS_SURFACE_PLAN.md`) and already calls `scheduler.place_with_spread`. The blocker is that placement queries the `CapabilityIndex`, which is empty unless capabilities have been announced and folded across the cluster. In a fresh harness with no daemons registered yet, the index is empty → placement returns `LocalPreferred` for everything → all members land on the same node → the chain looks degenerate.

**The fix has two parts.**

1. **Each cluster node announces a baseline capability set at boot.** `MeshOsDaemonSdk::start_with_verifier_and_migration_source` already does this on the substrate side; the harness needs to wait for the announcement to propagate before placement queries return useful results. The wait is folded into Phase 0's stabilization barrier (already polling for `peers.len() == n - 1`; extend to also require the local capability index has seen each peer's announcement).

2. **A small helper on `ClusterHarness` for the common case.** `harness.spawn_replica_group(kind, factory, members=3)` does the right thing: registers the factory on every node, spawns a `ReplicaGroup` of size 3 through node 0's runtime, lets `place_with_spread` distribute the members. Same shape as Missing item A but for groups instead of single daemons.

**Substrate work.** None to the placement primitive itself — `place_with_spread` already does what we need. The harness extension to wait for capability propagation is the only new code.

### Missing item C — Lifecycle coordination

**Goal.** Boot → ready → running → shutdown is one coherent state machine on the harness handle. No silent partial states; no `await`s that hang forever; no leaked tasks on drop.

**Why this is its own item.** Each of Phase 0 / item A / item B introduces its own readiness signal: "all peers handshook," "all daemons registered," "all chain members placed." A naive demo just `await`s them in sequence and crosses its fingers. A useful primitive owns the timeout, owns the rollback, and owns the shutdown path.

**API sketch.**

```rust
impl ClusterHarness {
    /// Drop every runtime, abort every spawned task, wait for
    /// the resulting joins. Idempotent. Called automatically on
    /// `Drop` but exposed so a test can await it explicitly.
    pub async fn shutdown(self) -> Result<(), ClusterError>;

    /// Health check usable by long-running demos: returns Ok(())
    /// iff every node still reports `peers.len() == n - 1` and
    /// every spawned daemon is still in `Running`.
    pub fn health(&self) -> ClusterHealth;
}
```

**The Drop story.** `ClusterHarness::Drop` calls `tokio::runtime::Handle::current().block_on(self.shutdown_sync())` — but only inside a tokio context. Outside one (panic during a test, abort path) the Drop logs a warning and detaches the runtimes; the OS cleans up the sockets. Same pattern `Mesh` uses today.

**Timeouts.** Every `await` in the harness has a budget: peer handshake 3 s, capability stabilization 2 s, daemon spawn 1 s per node, chain placement 3 s. Composable via a `ClusterConfig` for tests that want longer budgets.

**Substrate work.** None. This is policy + composition on top of primitives that already exist.

### Missing item D — nRPC observer hook on `Mesh`

**Goal.** Anyone holding a `Mesh` handle can install an `RpcObserver` that fires on every `serve_rpc_typed` / `call_typed` completion with the metadata the deck's NRPC tab wants — caller, callee, method, latency, status, request/response sizes. The deck demo's Phase 4 wires its `NrpcTail` push behind one such observer; the same hook is useful for tracing / metrics / debugging.

**Today's shape.** `Mesh::serve_rpc_typed` and `Mesh::call_typed` exist (the SDK / `nrpc_echo` example exercise them) but the dispatch path emits no observability events. Today's deck `NrpcTail` is fed by a synthetic seeder under `samples-logs` precisely because there's nothing to subscribe to.

**API sketch (illustrative, not final).**

```rust
/// Fired on each nRPC call boundary the local `Mesh` participates
/// in — either as caller (outbound) or callee (inbound). `latency_ms`
/// is wall-clock between request send and response receive on the
/// caller side; on the callee side it's the dispatch-to-respond
/// span. `status` carries success / typed-error / timeout / canceled.
pub trait RpcObserver: Send + Sync + 'static {
    fn on_call(&self, evt: RpcCallEvent);
}

pub struct RpcCallEvent {
    pub caller: NodeId,
    pub callee: NodeId,
    pub method: String,
    pub latency_ms: u32,
    pub status: RpcCallStatus,
    pub request_bytes: u32,
    pub response_bytes: u32,
    pub direction: RpcDirection,    // Outbound | Inbound
    pub ts_unix_ms: u64,
}

pub enum RpcCallStatus { Ok, Error(String), Timeout, Canceled }

impl Mesh {
    /// Install an observer. Replaces any previously-installed one.
    /// Pass `None` to clear. Cheap on the call path: one
    /// `ArcSwap::load` per call boundary; observer firings are
    /// `Box<dyn Fn>` calls inside the dispatch task.
    pub fn set_rpc_observer(&self, observer: Option<Arc<dyn RpcObserver>>);
}
```

**Substrate work.** New. Two surface changes:

1. **Carry the observer.** `Mesh` (or its inner type) grows an `ArcSwapOption<Arc<dyn RpcObserver>>` field. `set_rpc_observer` updates it. Cheap reads on the hot path.

2. **Fire from the dispatch path.** `Mesh::call_typed` records `Instant::now()` before sending, the inbound dispatcher records `Instant::now()` when invoking the user's handler. Both sides build an `RpcCallEvent` and call `observer.on_call(...)` if installed. Failure modes (timeout, codec error, cancellation) all funnel into the `RpcCallStatus` variants.

**Performance posture.** Observer firing is opt-in and cheap when not installed (one ArcSwap load → `None` → skip). Installed-but-busy observers run inline on the dispatch task; a slow observer slows nRPC dispatch. The expected use is a non-blocking push into a ring (the deck's `NrpcTail::push` already is) — anything heavier should be sketched against a bounded mpsc on the observer side. Same posture as `DeliverObserver` on `DaemonRuntime` (see [`SDK_GROUPS_SURFACE_PLAN.md`](SDK_GROUPS_SURFACE_PLAN.md) for the precedent).

**Why it's separable from items A–C.** A–C are pure SDK-composition primitives — no substrate surface change. D introduces new substrate API on `Mesh` and crosses the SemVer boundary. Reviewed independently. Lands as its own PR.

**Smoke test.** `tests/rpc_observer.rs`: spawn 2 nodes via the harness, install an observer on node 0 that pushes into a `Mutex<Vec<_>>`, have node 0 call a typed RPC on node 1, assert the observer fired once with `direction = Outbound, status = Ok`. Second case: same but installed on the callee, assert `direction = Inbound`. Third case: caller-side timeout, assert `status = Timeout`.

## Phases — dependency order

1. **Phase 0** — the multi-Mesh + multi-MeshOsRuntime harness skeleton. Standalone deliverable; smoke test demonstrates 5-node `Mesh` peering.
2. **Phase 0.5** — bridge probes. Depends on Phase 0. Smoke test extends to assert MeshOS-layer `snapshot.peers` populates.
3. **Item C (skeleton)** — `shutdown` + timeouts on Phase 0's `new`. Drop semantics. The "lifecycle" piece is interleaved with Phase 0 because they share the same state machine; it's listed separately because its surface is independently reviewable.
4. **Item A** — `spawn_per_node` + `spawn_where`. Depends on Phase 0 + 0.5 + item C.
5. **Item B** — `spawn_replica_group` and friends. Depends on Phase 0 + 0.5 + item C; reuses item A's rollback machinery internally.
6. **Item D** — `RpcObserver` + `Mesh::set_rpc_observer`. Independent of the harness internally (substrate-only change) but lands alongside because the deck demo's Phase 4 is the load-bearing consumer.

A reasonable PR sequence: **Phase 0 + Phase 0.5 + item C** as one PR (entangled — the lifecycle owns the boot path, the bridge probes wire during boot), item A as a second PR, item B as a third, item D as a fourth. Each PR ships with its own test under `crates/net/sdk/tests/cluster_*.rs` or `crates/net/tests/rpc_observer.rs`.

## Non-goals

- **No production multi-node SDK.** This is a `testing`-feature harness. It's not optimized, it doesn't tolerate handshake failures with retries, it doesn't survive a runtime crash. Production multi-node lives in user code; the harness is for tests + demos.
- **No new transport.** UDP loopback is the only transport. An in-memory channel transport would be faster but adds a code path that doesn't exist in production and would invalidate the harness's "real transport" claim.
- **No SDK semantic changes outside item D.** `MeshOsRuntime` / `DaemonRuntime` / `ReplicaGroup` / `Scheduler` keep their current behavior; items A–C are composition on top. Item D adds one new method to `Mesh` (`set_rpc_observer`) and one new trait (`RpcObserver`) — additive, no breakage.
- **No persistent state across runs.** The harness boots fresh every time. No on-disk redex / chain history. Tests that need persistence wire their own `RedexFile` paths through `MeshOsConfig`.
- **No observer batching / async dispatch.** `RpcObserver::on_call` is sync, fires inline on the dispatch task. Heavy observers must push into their own queue. Building an async observer trait or batching primitive is a future cleanup once a real consumer asks for it.

## Trade-offs to flag

- **Test ergonomics vs SDK surface.** Putting the harness behind a `testing` feature keeps it out of the release surface but means external library tests need to opt in via `[dev-dependencies] net-sdk = { …, features = ["testing"] }`. The alternative (`#[cfg(test)]`-only module) keeps it private but bars the deck demo from using it. **Recommend `testing` feature — the deck demo is the load-bearing consumer and `testing` is a common pattern.**
- **N-way capability stabilization is O(N²).** For N=5 that's 20 announcement records to wait for. Fine at demo sizes; a future "boot 50-node cluster for stress testing" would need a different barrier. Out of scope; flag it.
- **UDP port-allocation race.** Binding 0 → reading the assigned port → handing it to the next runtime is racy in theory (the kernel could reassign the freed port to an unrelated process between bind-and-close + the runtime's actual bind). In practice loopback ephemeral ports don't collide that fast. If we see flakiness, switch to "keep the socket open and pass the bound FD into the runtime" (already supported by the substrate's bind path).

## Open questions

1. **Module placement.** `net_sdk::testing::cluster` vs `net_sdk::cluster` (testing-feature-gated either way). Former is clearer about intent; latter is shorter. **Recommend `testing::cluster`.**

2. **Should the harness expose the underlying `Mesh` handles, or only `MeshOsDaemonSdk`?** Some tests want to poke `Mesh::serve_rpc_typed` directly; the deck demo never will. **Recommend exposing the `Mesh` via `node.mesh()` (already public on `MeshOsDaemonSdk`) — no extra harness surface needed.**

3. **Topology configurability.** Today full-mesh peering. A future test might want "ring" or "star" topologies to exercise routing. Out of scope for v1; the API takes a topology enum if/when needed.

4. **Operator identity for ICE actions in the demo.** The deck demo's operator identity needs to be one of the cluster's accepted signers. **Recommend: harness includes a `operator_identity()` helper that returns a deterministic keypair seeded from a fixed string; every node's verifier accepts this key in demo mode. Document that this is dev-only.**

## Locked decisions

- **Real UDP loopback only.** No in-memory channel transport.
- **`testing` Cargo feature, not `cfg(test)`.** External crates (including the deck binary's `demo` feature) need access.
- **N-way full mesh.** Every node peers every other node directly. No relay nodes, no NAT simulation.
- **Drop-safe shutdown.** Harness `Drop` is idempotent and never panics, even outside a tokio context.

## Deferred work

- **Topology variants.** Ring / star / partition-tolerant configurations. Out of scope until a test asks.
- **Cross-host harness.** Spawn N processes on N hosts. Distinct primitive; future plan if a multi-host demo or e2e suite comes along.
- **Failure injection.** "Drop this node's UDP packets to that node for 5 s" — useful for partition-tolerance tests. Out of scope; the harness exposes raw `Mesh` handles so a test can drive packet-loss simulation via the existing substrate hooks if it wants to.
- **Multi-observer dispatch.** Item D allows one `RpcObserver` per `Mesh`. A future "metrics observer AND tracing observer AND deck observer" composition would need either chained observers or an observer registry; out of scope until a second consumer beyond the deck demo exists.
