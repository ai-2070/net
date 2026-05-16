# Deck Demo Harness — implementation plan

> The substrate-side primitives [`DECK_DEMO_PLAN.md`](DECK_DEMO_PLAN.md) needs in order to boot a real in-process multi-node cluster instead of synthetic fixtures. Four pieces: **Phase 0** (in-process multi-`MeshOsRuntime` harness) + three missing items the deck demo can't build on top of yet — **A** daemon supervisor pattern across nodes, **B** real chain placement, **C** lifecycle coordination. This plan ships those primitives; the demo plan consumes them.

## Status

Design only. Today the SDK can boot **one** `MeshOsRuntime` cleanly (`MeshOsDaemonSdk::start_with_verifier_and_migration_source`, exercised by `deck/src/runtime.rs:117` and every integration test). It can boot **two** raw `Mesh` instances peered over UDP loopback (`crates/net/sdk/examples/nrpc_echo.rs:69-91`). It cannot boot **N** `MeshOsRuntime` instances peered together with a coherent lifecycle. Every prerequisite for `DECK_DEMO_PLAN.md`'s Phase 1 lives here.

## Frame

The SDK already has every load-bearing piece in isolation:

- `MeshOsRuntime` runs a single node's snapshot fold, scheduler, orchestrator, registry.
- `Mesh::connect_direct` opens a real UDP handshake against a known peer address.
- `Scheduler::place_with_spread` computes capability-spread placements once it can see peers in the `CapabilityIndex`.
- `ReplicaGroup` / `ForkGroup` / `StandbyGroup` (per `SDK_GROUPS_SURFACE_PLAN.md`) spawn coordinated daemon sets through a `DaemonRuntime` + scheduler.

What's missing is **glue**: a harness that stands N runtimes up, peers them by address, waits for the capability fold to converge, then hands the caller a typed handle for registering daemons + groups across all of them. The deck demo is the first consumer; the same primitive is useful for SDK integration tests today (multi-node groups, real migrations, real chain assignment) that are currently single-process / single-runtime workarounds.

## Why this exists

Three reasons this is a substrate-level plan, not deck-binary code:

1. **The harness is reusable.** "Boot N MeshOS runtimes in-process and run a scenario across them" is the shape every integration test that touches multi-node behavior wants. Putting it in `net_sdk::testing::cluster` (or similar) gives the deck demo + every future test the same primitive instead of growing parallel ad-hoc spawners.

2. **The supervisor / placement / lifecycle gaps are surface holes.** They're not deck-specific bugs. A library user trying to build a multi-node app today hits the same gaps — there's no idiomatic way to register a daemon factory "on every node in this group" or to spawn a chain whose members land via real placement. Closing the gaps closes them for everyone.

3. **Substrate work has its own review cadence.** Mixing "expose a new SDK helper" into a UX-focused deck demo plan obscures the substrate change. Split plans keep the SDK additions reviewable against substrate stability constraints (no semantic changes to chain placement, factory dispatch stays SemVer-stable, etc.).

## What ships

Four phases. Each is independently reviewable and lands a self-contained primitive.

### Phase 0 — In-process multi-`MeshOsRuntime` harness

**Goal.** A `ClusterHarness::new(n)` call returns N booted-and-peered `MeshOsDaemonSdk` handles in under 3 s on a developer laptop. Real UDP loopback, real handshake, real capability fold.

**API sketch (illustrative, not final).**

```rust
pub struct ClusterHarness {
    runtimes: Vec<MeshOsDaemonSdk>,
    addrs: Vec<SocketAddr>,
}

impl ClusterHarness {
    /// Spawn `n` runtimes on `127.0.0.1:<ephemeral>` and peer
    /// every pair via `Mesh::connect_direct`. Blocks until each
    /// runtime's capability fold reports `n-1` peers.
    pub async fn new(n: usize) -> Result<Self, ClusterError>;
    pub fn nodes(&self) -> &[MeshOsDaemonSdk];
    pub fn nth(&self, i: usize) -> &MeshOsDaemonSdk;
}
```

**Files.** `crates/net/sdk/src/testing/cluster.rs` (new module, gated behind a `testing` Cargo feature so it doesn't pollute the release surface). Re-exported as `net_sdk::testing::ClusterHarness`.

**Boot order.** (1) allocate N OS ports (bind 0 → keep socket, read assigned port); (2) build N `MeshOsConfig`s with `bind_addr` set to the allocated ports; (3) call `MeshOsDaemonSdk::start_with_verifier_and_migration_source` for each; (4) for each ordered pair `(i, j)` with `i < j`, call `runtime[i].mesh().connect_direct(addrs[j], identity[j])`; (5) poll every runtime's snapshot until `peers.len() == n - 1`, with a 5 s timeout.

**Where the substrate currently blocks.** Nowhere — every primitive used above already exists. The work is purely composition + the timeout-polling loop.

**Smoke test.** `tests/cluster_harness.rs`: boot 5 nodes, assert each one sees 4 peers within 3 s, drop the harness, assert clean shutdown (no leaked tokio tasks via `JoinSet::shutdown`).

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

## Phases — dependency order

1. **Phase 0** — the harness itself. Standalone deliverable; smoke test demonstrates 5-node peering.
2. **Item C (skeleton)** — `shutdown` + timeouts on Phase 0's `new`. Drop semantics. The "lifecycle" piece is interleaved with Phase 0 because they share the same state machine; it's listed separately because its surface is independently reviewable.
3. **Item A** — `spawn_per_node` + `spawn_where`. Depends only on Phase 0 + item C.
4. **Item B** — `spawn_replica_group` and friends. Depends on Phase 0 + item C; reuses item A's rollback machinery internally.

A reasonable PR sequence: Phase 0 + item C as one PR (they're entangled), item A as a second PR, item B as a third. Each PR ships with its own test under `crates/net/sdk/tests/cluster_*.rs`.

## Non-goals

- **No production multi-node SDK.** This is a `testing`-feature harness. It's not optimized, it doesn't tolerate handshake failures with retries, it doesn't survive a runtime crash. Production multi-node lives in user code; the harness is for tests + demos.
- **No new transport.** UDP loopback is the only transport. An in-memory channel transport would be faster but adds a code path that doesn't exist in production and would invalidate the harness's "real transport" claim.
- **No SDK semantic changes.** `MeshOsRuntime` / `DaemonRuntime` / `ReplicaGroup` / `Scheduler` keep their current behavior. Every new symbol is composition on top.
- **No persistent state across runs.** The harness boots fresh every time. No on-disk redex / chain history. Tests that need persistence wire their own `RedexFile` paths through `MeshOsConfig`.

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
