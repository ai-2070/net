# Deck Demo — implementation plan

> Replace the deck binary's `samples` + `samples-logs` synthetic fixtures with a single `demo` Cargo feature that boots a real in-process multi-node MeshOS cluster. Every deck tab observes natural steady-state instead of fabricated `PeerSnapshot` / `DaemonSnapshot` / log records. Sits on top of [`DECK_DEMO_HARNESS_PLAN.md`](DECK_DEMO_HARNESS_PLAN.md) (Phase 0 multi-node harness + supervisor / chain placement / lifecycle coordination primitives) — this plan consumes those primitives; the harness plan builds them.

## Status

Design only. Prerequisite: `DECK_DEMO_HARNESS_PLAN.md` Phases 0–3. Today the deck binary's `samples` feature wires four synthetic probes (`SampleLocalityProbe`, `SampleHealthProbe`, `SampleInventoryProbe`, `SampleMigrationSnapshotSource`) into a single `MeshOsRuntime` and folds in 17 fixture peers, 11 stub daemons, 8 hand-rolled chains, 4 in-flight migrations, and three in-memory `MeshBlobAdapter`s. `samples-logs` adds a 14-record log fixture on a 1.3 s timer + a 16-method nRPC seeder firing at 150 ms. None of it exercises real handshake, real capability broadcast, real migration phases, or the real chain machinery; the deck's tab renders are observing the substrate's snapshot reader but the data underneath is fabricated.

## Frame

The deck binary's purpose is to look like a real cluster. `samples` makes it *look* like a real cluster at a snapshot level — the tabs render, the counts are plausible, the lineage groups are recognizable — but a curious operator drilling in finds the chains are static, the migrations never advance, the log lines never reflect actual daemon work, and the dataforts contain blobs that were inserted at boot and never touched again. The demo flag closes that gap by booting an actual cluster, however small, on the same host.

The architectural posture: **one binary, one operator identity, N in-process MeshOS runtimes, real UDP loopback between them, real daemons producing real telemetry**. The deck's `runtime::spawn()` continues to exist for the single-node path (and stays the unflagged default); a new `demo::spawn()` boots the multi-node path under the `demo` feature.

## Why this exists

Three reasons this is worth a written plan:

1. **The fixture data has drifted from the substrate.** `MigrationSnapshot`, `ReplicaSnapshot`, `BlobInventoryEntry`, and `LogRecord` have all grown fields since the samples were first written. Each schema bump means another fixture record to hand-edit, another field that reads as "—" because nobody backfilled it. Real daemons folding through the real snapshot reader stay in sync automatically.

2. **The demo doesn't exercise the code paths it's demoing.** A migration that's a static `MigrationSnapshot` in a `Vec` never tests the orchestrator's 6-phase machine. A chain whose holders are hardcoded never tests `Scheduler::place_with_spread`. A log line published by a timer never tests the daemon log pipeline. The synthetic demo gives the wrong confidence: things look right on screen even when the underlying mechanics are broken.

3. **The split between `samples` and `samples-logs` is incidental.** The flags exist because logs were added later and the fixture's verbosity was disruptive at first. With a real cluster the log volume comes from real daemons; the separate flag stops being a useful axis. One `demo` flag is sufficient.

## What ships

Five phases. Each lands a vertical slice that's usable on its own (you can ship Phase 1 alone and have a real-cluster demo with empty NRPC + MIGRATIONS tabs; each subsequent phase fills in another tab).

1. **Phase 1 — Real daemons across real nodes.** Boot 5 MeshOS runtimes via the harness from `DECK_DEMO_HARNESS_PLAN.md`. Register four real `MeshDaemon` impls: `HeartbeatDaemon` (one per node — publishes log lines at natural daemon cadence, drives LOGS / MESH.EVENTS), `MixerDaemon` × 3 as a real `ReplicaGroup` (drives GROUPS / CHAINS), `DroneDaemon` × 3 as a real `ForkGroup` (drives the fork lineage flavor), `PyroSafetyDaemon` × 3 as a real `StandbyGroup` (drives the standby lineage flavor — 1 active + 2 warm, demonstrates `promote` on demand). The substrate folds them into `snapshot.daemons` naturally — no injection. Total ~14 real daemons across 5 real runtimes.

2. **Phase 2 — Real dataforts activity.** Each demo node attaches a real `MeshBlobAdapter` (the existing in-memory `Redex`-backed adapter — already real, the samples just don't use it from real nodes today). `HeartbeatDaemon` writes a small blob per tick and references it in its next log line. BLOBS and DATAFORTS tabs read live `BlobInventoryEntry` records produced by real `store_*` calls, not the boot-time fixture. The greedy-LRU + data-gravity instrumentation surfaces actual eviction / fetch counters.

3. **Phase 3 — Real migrations (v1, day-one).** A demo-side scheduler picks one daemon every ~30 s and calls `DaemonRuntime::migrate_to` against a peer node. The real `MigrationOrchestrator` drives the 6-phase machine (`Snapshot → Transfer → Restore → Replay → Cutover → Complete`); `MeshOsRuntime` folds the in-flight record into `snapshot.in_flight_migrations`. Replaces `SampleMigrationSnapshotSource` end-to-end. Operator can `[K]` from the MIGRATIONS tab to actually kill a real migration via ICE. Ships in v1 — no env-var disable knob; the demo is incomplete without it.

4. **Phase 4 — Real nRPC observation.** Depends on `DECK_DEMO_HARNESS_PLAN.md` Missing item D (substrate-level `RpcObserver` hook on `Mesh::call_typed` / `serve_rpc_typed`). Two demo-side pieces land here: an `RpcChatterPair` (a tiny `requester` daemon that periodically calls a `responder` daemon's typed RPC method) wired across 4 of the 5 nodes so calls flow over real handshake-encrypted UDP, and the deck's `NrpcTail` re-pointed at the observer hook instead of the synthetic seeder. NRPC tab populates from real call records — caller, callee, method, latency, status, byte counts — produced by the substrate's actual nRPC dispatch path.

5. **Phase 5 — Cargo + cleanup.** Remove `samples`, `samples-logs`, the `samples_logs` module, and every `Sample*Probe` / fixture constant from `deck/src/runtime.rs`. Add `demo` (off by default; `cargo run --features demo`). Move the in-memory `Redex` adapter setup that's currently flag-gated into the always-on harness path so the single-node default ships with a working dataforts surface. Update `main.rs` to branch on the feature: `demo::spawn()` under the flag, `runtime::spawn()` otherwise.

## What this doc does NOT ship

🚫 **Not a real distributed cluster.** All nodes share one OS process. Suitable for "show me the deck UI against real-looking telemetry," not for load testing or network-fault scenarios. UDP loopback is real transport but every runtime crashes together.

🚫 **Not failure-injection.** A "canary daemon that crashes every 90 s" would populate the FAILURES tab. Out of scope for v1 — operators who want to see the failures path can `cargo run --features demo` and then hit `[K]` on a migration or `[F]` to force-freeze a node; the substrate emits real failure records from real ICE actions.

🚫 **Not a release-mode deliverable.** `demo` is for development and operator-facing demos. It does not ship to crates.io as a default feature. The release manifest stays minimal.

## Phases

### Phase 1 — Real daemons (the load-bearing slice)

**Goal.** A 5-node `cargo run --features demo` boots a real cluster, registers real daemons, and every deck tab — LOGS, NODES, DAEMONS, GROUPS, CHAINS, NET.MAP — renders live data within ~5 s of startup. (NRPC + MIGRATIONS populate in Phases 3–4.)

**Depends on.** `DECK_DEMO_HARNESS_PLAN.md` Phase 0 (multi-node harness) + Missing-item A (daemon supervisor) + Missing-item B (real chain placement) + Missing-item C (lifecycle coordination).

**Files this phase adds.**
- `deck/src/demo/mod.rs` — feature-gated module, re-exports `spawn` + the demo `Harness` type.
- `deck/src/demo/cluster.rs` — calls into the harness from `DECK_DEMO_HARNESS_PLAN.md`; configures the 5-node topology + port pool.
- `deck/src/demo/daemons.rs` — `HeartbeatDaemon`, `MixerDaemon`, `DroneDaemon`, `PyroSafetyDaemon` impls.
- `deck/src/demo/spawn.rs` — orchestrates boot order: harness up → peer mesh stabilized → daemons registered → groups spawned → return a `Harness` to `main.rs`.

**Daemons in concrete terms.**
- `HeartbeatDaemon`: one per node (5 total). `process()` is a no-op (nothing inbound). A tokio task spawned alongside it calls `publish_log` at natural daemon cadence — roughly every 800 ms with jitter, varied messages drawn from a per-node corpus so the LOGS tab doesn't read identically across nodes. Total LOGS rate: ~6 lines/s; intentionally verbose.
- `MixerDaemon`: a `ReplicaGroup` of 3 members placed across 3 of the 5 nodes by `Scheduler::place_with_spread`. The group's factory builds members with deterministic keypairs (`group_seed + index`). `process()` handles a `MixCommand` event the demo publishes every ~3 s; routes round-robin across members. Drives the GROUPS tab's replica row and the CHAINS tab's first chain.
- `DroneDaemon`: a `ForkGroup` of 3 forks from a common parent at `fork_seq = 7`. Each fork's identity is unique. `process()` is a no-op. Populates the GROUPS tab's fork-lineage tag and the second chain in the CHAINS tab.
- `PyroSafetyDaemon`: a `StandbyGroup` (1 active + 2 warm) placed across the remaining nodes. `sync_standbys(...)` runs at ~10 s cadence so the standby `synced_through` advances visibly. Operator can hit `[P]` (TBD binding) to trigger a real `promote` and see the active swap in the GROUPS tab.

**Boot expectations.** From `cargo run --features demo` to fully-stabilized cluster: < 5 s. The deck shows a "booting demo cluster… (N/5 nodes ready)" splash while the harness comes up.

### Phase 2 — Real dataforts activity

**Goal.** BLOBS and DATAFORTS tabs render live records driven by real `store_*` calls, not boot-time fixture inserts.

**What lands.**
- Each demo node attaches one `MeshBlobAdapter` (the existing in-memory `Redex`-backed kind). Adapter IDs follow the existing samples convention (`deck-samples`, `cold-storage`, `replicated`) so screenshots stay recognizable, but each one is now anchored to a real node.
- `HeartbeatDaemon`'s tokio task is extended: every ~3 s it calls `adapter.store(blob)` with a small (~256 B) payload constructed from `(node_id, tick)`. The `BlobInventoryEntry` rows accumulate naturally and the greedy-LRU sweep eventually fires once the per-adapter cap is reached.
- One node's adapter gets a much smaller cap (256 KB say) so overflow + sweepable / quiet status transitions are visible without waiting hours.

**What we don't add.** Cross-node blob replication is a real `Redex` feature but the demo doesn't drive it directly — chains already cause replicas to land via the group's replicated state. If `r.replica_target` ends up `None` on BLOBS rows in practice, accept it; that's an honest reflection of the surface.

### Phase 3 — Real migrations

**Goal.** MIGRATIONS tab observes real in-flight migrations driven by `MigrationOrchestrator::migrate_to`. The 4-record `SampleMigrationSnapshotSource` is deleted; the production `OrchestratorMigrationSnapshotSource` is wired in its place. Day-one v1 deliverable — the demo is not feature-complete without it.

**What lands.**
- `deck/src/demo/migrator.rs` — a tokio task that, every ~30 s, picks one `HeartbeatDaemon` instance and calls `runtime.migrate_to(daemon, target_node)`. Target is picked round-robin across non-source nodes. Resolves the daemon → target NodeId via `Scheduler::query`.
- The substrate's existing `MigrationOrchestrator` drives the 6 phases. `MeshOsRuntime` folds in-flight records into `snapshot.in_flight_migrations` automatically — same path production uses.
- Operator can `[K]` on a cursored migration to kill it. ICE commit goes through the real signing path against the operator's demo-identity. After a kill, the substrate emits a real `MigrationFailureReason::CanceledByAdmin` record that lands in the FAILURES tab.

**Cadence trade-off.** 30 s is "long enough that the operator sees the migration progress through phases on screen, short enough that there's always one in flight to look at." Shorter and the MIGRATIONS tab is constantly busy; longer and the demo reads as quiet. Tunable via a constant in `demo/migrator.rs`.

### Phase 4 — Real nRPC observation

**Goal.** NRPC tab populates from real `Mesh::call_typed` / `serve_rpc_typed` traffic flowing across the cluster, observed via the substrate-level `RpcObserver` hook from `DECK_DEMO_HARNESS_PLAN.md` Missing item D.

**Depends on.** `DECK_DEMO_HARNESS_PLAN.md` Missing item D (`RpcObserver` trait on `Mesh`, fired on each call's send + receive boundary, carries `(caller, callee, method, latency_ms, status, request_bytes, response_bytes)`).

**What lands on the demo side.**
- `deck/src/demo/rpc_chatter.rs` — defines `RpcChatterDaemon` in two roles: a *responder* registered on 2 nodes that serves a small typed RPC surface (`echo`, `ping`, `metrics_snapshot`), and a *requester* registered on the remaining 3 nodes that fires a call against a random responder every ~250 ms. Round-robin method selection across the surface so the NRPC tab shows method-level diversity.
- `deck/src/streams.rs::NrpcTail` wired to the new observer: a small bridge `impl RpcObserver` pushes each completed call into the existing `NrpcTail` ring, replacing the `samples-logs` synthetic seeder entirely. The bridge installs on every node's `Mesh` at harness boot.
- Status mix is whatever the real RPC dispatch produces — `Ok` is the default; `Error` records show up on the rare boot-race calls before the responder side is fully registered, and (per Phase 3) on calls aimed at a daemon that's mid-migration. No fabricated error distribution.

**Per-node call volume.** 3 requesters at 250 ms each = ~12 calls/s observed. Roughly the same density the synthetic seeder produced (~6/s) but real. If the LOGS tab becomes drowned by mesh chatter during early testing, requester cadence is bumped down. Tunable via a constant in `demo/rpc_chatter.rs`.

### Phase 5 — Cargo + cleanup

**Cargo edits in `deck/Cargo.toml`.**
- Delete `[features] samples = [...]` and `samples-logs = ["samples"]`.
- Add `demo = []`. Empty feature list — the demo wires real types that are already in the `default` dependency closure (`net-sdk`/{compute, groups, dataforts, meshos, deck}).
- Documentation comment block above the new feature pinning what it does + that it's not for release.

**Code edits.**
- Delete `samples_logs` module + every `Sample*Probe` and `SampleDaemon` in `deck/src/runtime.rs`.
- Delete `runtime::spawn_nrpc_seeder` and every synthetic-injector path — Phase 4 replaced them with the real `RpcObserver` bridge feeding `NrpcTail`.
- Move the in-memory `MeshBlobAdapter` wiring that was `samples`-gated into the always-on path with adapter-list = empty. The single-node default ships with the dataforts surface compiled in (already the case) but no adapters wired by default; the user wires their own.
- `main.rs` branches: `#[cfg(feature = "demo")] let harness = demo::spawn().await?;` else `let harness = runtime::spawn().await?;`.
- The `NrpcTail` survives but stays observer-driven. Non-demo single-node builds render it empty until the operator wires their own observer (clean shape; no fixture).

**Doc edits.** `DECK_FEATURES.md` and the deck `README.md` get a section explaining the demo flag. `DECK_PLAN.md`'s opening status line mentions the demo flag exists.

## Locked decisions

- **5 nodes.** Hardcoded in v1 — no env-var override. Five is enough room for `ReplicaGroup` × 3, `ForkGroup` × 3, and `StandbyGroup` (1 active + 2 warm) without contention, with diverse chain holders and an uncluttered NET.MAP. Costs (RAM / ports / tokio tasks) acceptable on any dev laptop. Boot time budget: < 5 s.
- **Real migrations day-one.** Phase 3 ships in v1. No env-var disable knob — the demo is incomplete without it. Initial flakiness from RTT jitter or snapshot-size issues is treated as a bug to fix, not a feature to gate.
- **Real logging, verbose-OK.** `HeartbeatDaemon` emits at natural cadence (~800 ms with jitter, ~6 lines/s across the 5 nodes). No artificial throttling. If the LOGS tab reads as noisy, the answer is to lean on the existing filter bar / pause toggle, not to slow the daemons.
- **Real `RpcObserver`, not a fake.** Phase 4 builds the substrate-level observer hook (`DECK_DEMO_HARNESS_PLAN.md` Missing item D) and wires `NrpcTail` to it. The synthetic seeder is deleted. NRPC tab populates from real traffic only.
- **One process, N runtimes.** Multi-process demos (each runtime as its own OS process, deck attaches via remote `DeckClient`) are out of scope — that's the multi-cluster slice's territory.
- **UDP loopback.** Each runtime gets its own ephemeral port on `127.0.0.1`. No in-memory channel transport — the real handshake / capability broadcast / scope enforcement run end-to-end.
- **Real signing.** ICE actions from the deck use a demo operator identity (deterministic keypair seeded from a fixed string). The signed-commit path is the real one; the identity is just convenient.
- **Demo flag is dev-only.** Not in any default-features set, not in `local`, not in `full`. Pulled in only by explicit `--features demo`.

## Deferred work

- **Cross-host demo cluster.** A `demo` mode that spawns N processes across a small inventory of hosts (for a "real" datacenter feel during demos) is a future option. Reuses everything from this plan but the harness picks real IP addresses instead of `127.0.0.1`.
- **Demo-side stress knobs.** "Spawn 100 daemons", "rotate chain leaders every 5 s", "simulate node loss" — operator-facing demo controls. Out of scope for the first cut; add once the v1 demo is in operators' hands and a real ask emerges.
- **Failure-injection canary.** A daemon that crashes on schedule to drive the FAILURES tab without operator ICE actions. Deferred per [§What this doc does NOT ship](#what-this-doc-does-not-ship).
