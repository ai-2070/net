# Code review — `subnet-scaling` branch, pass 2 (2026-05-23)

Branch base: `master`.
Scope: the ~20 commits that landed AFTER the first review pass
(`CODE_REVIEW_2026_05_23_SUBNET_SCALING.md`). Adds the
`adapter/net/behavior/lifecycle/` module (daemon / group / monitor),
`aggregator/{registry,registry_service,registry_client}`, a turnkey
aggregator-daemon binary, CLI `aggregator ls / spawn / scale`, and the
`AggregatorRegistry` snapshot accessor on `DeckClient`. ~5,400 LOC
across 25 files.

Three review agents (reuse / quality / efficiency) were dispatched in
parallel. Findings below are organised by severity, then category. File
paths are relative to repo root; line numbers reflect the branch tip
and may drift.

---

## HIGH — correctness / concurrency / duplication risks

### S1 — `snapshot_group` lock storm + duplicated build loop

`net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:284-309`
and `net/crates/net/src/adapter/net/behavior/deck.rs:892-942`.

Both surfaces (RPC handler + DeckClient accessor) reimplement the same
per-replica loop: `entry.replicas() / placements() / health()`, zip,
fall back to a hand-built `ReplicaHealth` default when an index is
missing, then build a structurally-identical per-replica row (only the
concrete type differs — `RegistryReplicaSummary` vs
`AggregatorReplicaRow`).

Worse: each call takes **three sequential async-lock acquisitions** on
`AggregatorGroupEntry`'s `AsyncMutex<Option<LifecycleGroup>>`. For
operator polling at 1 Hz across N groups of K replicas, that's 3 × N
lock round-trips and N × (2 × K + sizeof(health) × K) allocations per
second, blocking any concurrent `register`/`unregister` writer for the
duration.

Fix: add `AggregatorGroupEntry::snapshot()` that locks once, builds the
full set of rows inline, and returns either typed rows or the wire
summary. Both call sites consume it.

### S2 — `HealthMonitor` holds the group's async mutex across every replica's `health().await`

`net/crates/net/src/adapter/net/behavior/lifecycle/monitor.rs:221-233`
calling into `lifecycle/group.rs:305-311`.

`LifecycleGroup::health()` runs `join_all` over per-replica
`r.health().await` futures while the outer `g.lock().await` guard is
still held. Every `List` RPC, every accessor
(`replica_count` / `replicas` / `placements`), and every `unregister`
blocks the entire monitor pass. With the aggregator-daemon's default
`4 × summary_interval_ms` cadence, a 1 s interval stalls operator reads
by a few ms per group during each poll.

Fix: snapshot `replicas` (cheap `Vec<Arc<L>>`) under the lock, drop the
guard, then `join_all` the health checks outside. `replace` still
re-locks as today.

### S3 — `last_tick_at: Mutex<Option<Instant>>` is redundant with `generation` + `summary_interval`

`net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:155, 244,
274, 310, 559-573`.

Three tick variants each write-lock the mutex; `health()` read-locks
it; all to compute `elapsed > 3 × interval`. The daemon owns the
ticker, so the same answer is derivable from a single `Instant` stamped
at `new()` time plus the existing `generation` counter:
`expected_ticks = start.elapsed() / interval` compared against
`generation()`.

Fix: drop the `Mutex<Option<Instant>>` field. Store one
`start_instant: Instant` at construction; have `health()` derive
liveness from it. Removes the field, the read/write-lock contention,
and three write sites.

### S4 — `RegistryClient` end-to-end duplicates `FoldQueryClient`'s shape

`net/crates/net/src/adapter/net/behavior/aggregator/registry_client.rs:1-250`
vs
`net/crates/net/src/adapter/net/behavior/aggregator/query_client.rs:1-250`.

Identical structure: `Transport/Codec/Server` error enum, `From<RpcError>`
+ `From<postcard::Error>` impls, `with_deadline` builder,
`DEFAULT_*_DEADLINE` constant, and an `issue_call` / `send` helper that
does `postcard::to_allocvec → mesh.call(opts) → postcard::from_bytes`.

Fix: hoist a `MeshRpcClient<E>` (or at minimum a
`send_typed<Req, Resp>(target, service, op, deadline)` helper) into
`adapter/net/mesh_rpc/`. Both clients become thin typed wrappers. New
RPC services pick it up for free.

### S5 — `aggregator-daemon::parse_subnet` duplicates `SubnetId::FromStr`

`net/crates/net/aggregator-daemon/src/lib.rs:630-650` plus 4 tests at
`:656-675`.

Same dotted-level + `global` + `MAX_DEPTH` semantics that L7 hoisted
onto `SubnetId::FromStr` last pass.

Fix: `raw.trim().parse::<SubnetId>()`. Drop the local fn and its tests.

### S6 — `decode_psk` / `decode_seed` / CLI `hex_decode_32` reinvent `hex::decode`

`net/crates/net/aggregator-daemon/src/lib.rs:576-599` and
`net/crates/net/cli/src/commands/aggregator.rs:316-327`.

Byte-by-byte loops where `hex::decode` already does the work. `hex` is
a direct dep of both crates (`cli/Cargo.toml:70`,
`aggregator-daemon/Cargo.toml:53`) and is used in
`cli/src/commands/{ice,identity,context}.rs` for the same shape.

Fix: one `hex::decode(trimmed).and_then(|b| b.try_into())` helper.

### S7 — `derive_seed_from_name` uses `DefaultHasher`

`net/crates/net/aggregator-daemon/src/lib.rs:605-628`.

`DefaultHasher` is documented as not stable across Rust releases.
Operators upgrading the daemon binary will silently get different
derived seeds → different replica identities → fold-state churn on
upgrade. This is the kind of bug that surfaces a quarter after the
release goes out.

Fix: `blake3::hash` or `siphasher` with explicit, repo-pinned keys.

---

## MEDIUM — quality / hygiene

### S8 — `make_spawner` + `spawn_group` are near-duplicate 60-line blocks

`net/crates/net/aggregator-daemon/src/lib.rs:460-532` (`make_spawner`)
vs `:326-423` (`spawn_group`).

Both build an `AggregatorConfig`, call `LifecycleGroup::spawn`, set up
the monitor factory, and call `register_with_monitor`. Only the source
of fields differs (`GroupConfig` vs `TemplateConfig`).

Fix: collapse `GroupConfig` and `TemplateConfig` into a shared
`AggregatorSpec { name, source_subnet, fold_kinds, interval_ms,
replica_count }`. Extract `spawn_and_register(spec) -> Result<...,
RegistryRpcError>` used by both paths.

### S9 — `RegistryRpcError::SpawnNotSupported` is a runtime stand-in for a missing type split

`net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:143-251`.

`RegistryHandler::new()` builds a handler that rejects half its own API
at runtime. Clients only discover the asymmetry on the first `Spawn`.

Fix: split into `RegistryReadHandler` (List / Unregister) and
`RegistryHandler` (adds Spawn, requires a `SpawnFn`). Type-level rather
than runtime guarantee. The wire-error variant deletes itself.

### S10 — `make_spawner` panics via `expect("aggregator config validated")` in four places

`net/crates/net/aggregator-daemon/src/lib.rs:386-391, 406-411, 498-504,
511-517`.

Closures assert config validated upstream. The pre-validation only
happened in one path (`spawn_group`); for the `Spawn` RPC payload's
`with_fold_kind` loop there's no validation between the template lookup
and the factory invocation. A bad template `fold_kinds` panics the
spawner task instead of returning a typed error to the client.

Fix: validate templates eagerly at `boot()` time (call
`AggregatorDaemon::new` once with the resolved cfg + a throwaway mesh
at config-load time), or surface the validation error properly inside
the factory.

### S11 — `aggregator-daemon` boot spawns groups sequentially

`net/crates/net/aggregator-daemon/src/lib.rs:307-310`.

`for group_cfg in &config.groups { spawn_group(...).await? }`. Each
`spawn_group` parallelizes its replicas via `join_all` internally
(good), but groups themselves are sequential. Boot of N groups of M
replicas each takes ~N × max(per-replica start latency) instead of
~max.

Fix: `futures::future::try_join_all(config.groups.iter().map(|g|
spawn_group(&registry, &mesh, g)))`. One-line change.

### S12 — `set_aggregator_registry` doc says "call before start" but nothing enforces it

`net/crates/net/src/adapter/net/mesh.rs:5324-5340`.

Pointer reads stay racy with channel-publish initialization if the
registry is installed after `start()`. Currently a doc-comment
constraint with no compile- or runtime-time enforcement.

Fix: `debug_assert!(!self.is_started())`, or accept the registry in
`MeshNodeConfig` so it can only be set at construction.

### S13 — `HealthMonitor::spawn` (vs `spawn_with_option`) is misleading dead surface

`net/crates/net/src/adapter/net/behavior/lifecycle/monitor.rs:136-163`.

Only `spawn_with_option` is wired in production (via
`register_with_monitor`). `spawn` + the `MonitorGroupRef::Plain` arm in
the poll loop exist solely to keep two tests happy, with a 15-line
comment explaining why `spawn` "keeps its original behavior."

Fix: delete `spawn` and the `Plain` arm; rewrap the two `monitor.rs`
tests in `Arc<AsyncMutex<Option<_>>>`. Removes ~60 lines of
explanation about a distinction that doesn't matter to callers.

---

## MEDIUM — efficiency / reuse

### S14 — `LifecycleGroup::replicas()` returns full `Vec<Arc<L>>` clone per call

`net/crates/net/src/adapter/net/behavior/lifecycle/group.rs:281-283`.

Called by `entry.replicas()`, which is itself called by `snapshot_group`
per group per List RPC. Combined with S1 above, one Vec alloc + K Arc
clones per group per RPC.

Fix: add `fn replicas_slice(&self) -> &[Arc<L>]` and have
`snapshot_group` consume that under its single lock guard from S1.

### S15 — `HealthMonitor` re-replaces unhealthy replicas every tick with no backoff

`net/crates/net/src/adapter/net/behavior/lifecycle/monitor.rs:32-34`
(already documented).

A persistently-failing replica burns a `LifecycleGroup::replace`
(`old_handle.stop().await + new_handle.start().await` while holding the
lock — see S2) every interval. Real cost: contention storm during a
bad replica plus N pointless `on_start` / `on_stop` cycles per minute.

Fix: track per-index consecutive failures in `HealthMonitorStats`; skip
with exponential backoff after the first failed replace.

### S16 — `install_registry_service` repeats `install_query_service` shape

`net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:309-345`
vs
`net/crates/net/src/adapter/net/behavior/aggregator/query_service.rs:234+`.

Both are 1:1 `mesh.serve_rpc(SERVICE_CONST,
Arc::new(Handler::new(self.clone())))`. Small duplication, but worth a
shared `install_rpc_service<H: RpcHandler>(mesh, service_name,
handler)` helper on `MeshNode` so future services don't keep adding
their own.

---

## LOW — typing / cosmetic

### S17 — Stringly-typed `group_name` / `template_name` / `name`

Throughout `registry.rs`, `registry_service.rs`, `registry_client.rs`.

All three are the same shape and swappable at the type level. A client
that swaps `group_name` and `template_name` in a `Spawn` call gets
`UnknownTemplate("primary")` at runtime rather than a compile error.

Fix (low priority): newtype wrappers (`GroupName(String)`,
`TemplateName(String)`) lock this down. Cross-cutting migration so
worth its own slice rather than rolled into this pass.

### S18 — Heavy WHAT-narrating doc comments

Examples: `daemon.rs:185-194` ("Spawn the background summarize loop
and return its JoinHandle. The handle resolves when the loop
exits…"), `monitor.rs:152-161` (15-line "we can't move out of the
existing `Arc<AsyncMutex<...>>`"). Most explain code visible one line
below.

Fix: trim to one-line summaries; preserve WHY paragraphs (e.g.
`daemon.rs:537-545` on_stop backstop-timeout reasoning is genuinely
non-obvious and should stay).

### S19 — `aggregator-daemon` config has no `[gateway]` / peer-config / per-group `summary_visibility`

`net/crates/net/aggregator-daemon/src/lib.rs:88-110`.

Operators deploying multi-subnet meshes will need at minimum the peer
list at boot. Acceptable for this slice (shipped per
`AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`); flag for the next
slice.

### S20 — `AggregatorGroupEntry` exposes `name` / `group_seed` as fields but gates `group` behind an async lock

`net/crates/net/src/adapter/net/behavior/aggregator/registry.rs:63`.

Partial encapsulation — half the entry is `pub field`, half is
`pub async fn`. Ergonomics fine today; flag for the next pass that
touches the entry.

---

## False positives noted during the pass

- **Registry concurrency under `RwLock`:** `register`/`unregister` use
  `write()` and `entries`/`get`/`names` use `read()`. Correct. The
  perceived contention is inside the per-entry async mutex (covered by
  S1/S2), not the outer map.
- **`derive_seed_from_name` "duplicates `derive_replica_keypair`":**
  different purpose (group-seed vs per-replica keypair). The
  `DefaultHasher` complaint is S7; the choice of a separate function
  is fine.
- **`HealthMonitor` duplicates `RecoveryRegistry`:** different shape
  (tick-driven sync closures over `MeshDaemon` vs self-driven async
  interval over `LifecycleDaemon`). Bridging them needs the deferred
  trait-merge work from the prior review's L3.

---

## Clean areas

- **`aggregator_registry_rpc.rs` integration test:** no fixed sleeps,
  no brittle waits — uses `await accept_task`. Solid.
- **`LifecycleGroup::spawn` parallelizes via `join_all`** internally
  (the prior review's E2 work landed and stuck).
- **`derive_replica_keypair` reuse** in `LifecycleGroup` is correct.
- **`AggregatorRegistry`'s `RwLock<HashMap>`** choice is correct given
  the read-heavy access pattern (List vs occasional Spawn /
  Unregister).

---

## Suggested fix order

1. **S1, S2, S3** — concurrency wins; all local, no new abstractions.
2. **S5, S6, S7, S11, S12** — trivial fixes / one-liners.
3. **S10** — eagerly-validate templates at boot (correctness bug).
4. **S4** — `MeshRpcClient` hoist (collapses S4 + S16 in one stroke).
5. **S8, S9, S13** — surface tightening, ~150 LOC delta total.
6. **S14, S15** — depend on S1/S2 / S15 is its own slice (backoff
   policy).
7. **S17, S18, S19, S20** — defer to a dedicated cleanup pass or the
   next slice.
