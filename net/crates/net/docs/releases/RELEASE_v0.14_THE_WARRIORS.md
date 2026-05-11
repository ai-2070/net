# Net v0.14 — "The Warriors"

*Named after Walter Hill's 1979 cult film and Rockstar Games' 2005 adaptation — a gang trying to make it home through hostile turf. Channels in this release do the same: replicas survive partitions, election storms, disk pressure, and divergent tails, and still converge on a consistent leader before the night is out.*

v0.14 lands **cross-node replication for RedEX channels** end-to-end across the substrate and all five bindings. v0.13 ("Chippin' In") made capability the load-bearing layer; v0.14 makes replication the load-bearing layer underneath the channel surface. `SUBPROTOCOL_REDEX` is now a real wire codec, `ReplicationCoordinator` is a real tokio runtime task with a 4-state machine pinned per plan §3, leader election is deterministic nearest-RTT with a NodeId tiebreak (no broadcast, no epoch — microseconds-wide convergence), and catch-up is pull-based with bandwidth budgets and a 64 MiB hard ceiling. Every binding exposes the same `enable_replication(mesh)` / `open_file(name, cfg.with_replication(Some(rep)))` surface and the same per-channel Prometheus snapshot.

The hardening posture from the Black Diamond line continues — every new surface ships with handle-lifetime, panic-safety, FFI-soundness, lock-order, and cancel-safety guarantees consistent with v0.11 / v0.12 / v0.13 — and a sixty-four-item second-pass review (`docs/misc/CODE_REVIEW_2026_05_11_REDEX_DISTRIBUTED.md`) shipped its closure commits before the v0.14 branch cut.

Alongside the replication landing, v0.14 carries two cross-cutting breaking changes: capability hardware / network units switch from MB / Mbps to GB / Gbps end-to-end, and the predicate-on-the-wire header renames from `cyberdeck-where:` to `net-where:` (predicate envelope ABI bumped to `2`).

---

## RedEX Distributed (substrate)

The implementation plan in [`REDEX_DISTRIBUTED_PLAN.md`](../plans/REDEX_DISTRIBUTED_PLAN.md) phases A–I all closed before v0.14. The shape:

### `ReplicationConfig`

```rust
pub struct ReplicationConfig {
    pub factor: u8,                       // replicas including leader; 1..=16, default 3
    pub placement: PlacementStrategy,     // Standard / Pinned([NodeId]) / ColocationStrict
    pub heartbeat_ms: u64,                // 100..=300_000, default 500
    pub leader_pinned: Option<NodeId>,    // pin election outcome to a specific NodeId
    pub on_under_capacity: UnderCapacity, // Withdraw (default) / EvictOldest
    pub replication_budget_fraction: f32, // share of measured NIC peak; 0.0 < f ≤ 1.0
}
```

`PlacementStrategy::Standard` defers to the v0.13 `PlacementFilter` axes (scope filter, proximity max-RTT, capability intent matching, anti-affinity, custom-filter callback). `Pinned([NodeId])` and `ColocationStrict` skip the filter chain. `UnderCapacity::Withdraw` (the default) drops the replica role and lets the leader's other replicas absorb the redundancy responsibility; `EvictOldest` runs `RedexFile::sweep_retention` against the configured caps and stays in `Replica`. `validate()` enforces every invariant at construction; binding layers run it before crossing the FFI so a malformed config can't leak into the coordinator's hot loop.

### Wire protocol — `SUBPROTOCOL_REDEX`

A new subprotocol family at `0x0E00`. Four message types pinned at byte-level:

- `SYNC_REQUEST` (`0x20`, replica → leader) — fixed-size `{ channel_id, since_seq, chunk_max }`.
- `SYNC_RESPONSE` (`0x21`, leader → replica) — variable; carries `{ channel_id, first_seq, leader_first_retained_seq, events: [{event_seq, payload_len, payload}] }`. The new `leader_first_retained_seq` field lets the replica disambiguate retention-trim from split-brain divergence; legitimate trim with `first_seq == leader_first_retained_seq` triggers skip-ahead via `RedexFile::skip_to`, any other gap shape NACKs back and bumps `dataforts_replication_skip_ahead_total`.
- `SYNC_HEARTBEAT` (`0x22`, bidirectional) — fixed-size `{ channel_id, tail_seq, role, wall_clock_ms }`. Pinned at 52 bytes; the role byte is the validator-checked `ReplicaRole` discriminant.
- `SYNC_NACK` (`0x23`, leader → replica) — variable; carries `{ channel_id, since_seq, error_code, detail_len, detail }`. Error codes: `1 NotLeader` / `2 BadRange` / `3 Backpressure` / `4 ChannelClosed`. `detail` truncates at a UTF-8 char boundary ≤ `u16::MAX` so a multi-byte codepoint straddling the cap can't ship invalid UTF-8 to the peer.

Codec is hand-rolled (no serde over the wire) for byte-stable round-trips, validated by `byte_layout_pinned` tests per message type. Truncation errors carry `(need, have)` for diagnostics; `need = consumed + still_needed` so a peer logging the value sees an accurate frame-completion estimate.

### `ReplicationCoordinator` — the 4-state machine

```rust
pub enum ReplicaRole { Idle, Replica, Candidate, Leader }
```

Transitions are matrix-validated and serialized through an outer `tokio::sync::Mutex<()>` so the state write + chain-tag side-effect (`announce_chain` / `withdraw_chain` against `MeshNode`) can't interleave. Two `transition_to` calls racing one another produce a deterministic sequence: T1's `Replica → Candidate` announce never lands after T2's `Idle` withdraw. The transition signals (`CapabilitySelected`, `MissedHeartbeats`, `ElectionWon`, `ElectionLost`, `GracefulRelinquish`, `DiskPressureWithdraw`, `ChannelClose`) are pinned per plan §3; `ChannelClose` is the universal escape valid from any state, used by the disk-pressure / channel-closed paths when the current role isn't `Replica`.

The coordinator surfaces two error variants:

- `CoordinatorError::Transition` — the validator rejected the triple. State unchanged.
- `CoordinatorError::TagSink` — the state mutation already happened; only the chain-tag side-effect failed. Operator observes a divergence between local state and advertised state until the next successful announce. Runtime handlers clear the believed leader on both variants so the next tick re-enters discovery cleanly.

### Replica selection vs. leader election

Two distinct subsystems per plan §4:

- **Placement** consults `PlacementFilter` to choose which N nodes carry the channel's replica set when the channel is first opened or on roster change. `Standard` flows through the v0.13 scoring; `Pinned` skips it. The selected set is published via the `causal:<hex>` chain-tag layer so peers discover holders without a centralized membership view.

- **Leader election** is a pure function over each healthy replica's locally-known state:

```text
elect(replica_set, self_id, rtt_to, health_of) -> ElectionOutcome:
    R = { r ∈ replica_set : health_of(r) }
    sorted = R sorted by (rtt_to(self, r), r.node_id_lex)   // tie-break: lexicographic NodeId
    return ElectionOutcome::PeerWins(sorted[0])
        | ElectionOutcome::SelfWins
        | ElectionOutcome::NoEligibleReplica
```

No broadcast, no epoch, no collection window. Every healthy replica computes the same winner from the same `(replica_set, self_id, rtt_to, health_of)` tuple, so leader-loss recovery converges in microseconds without the wire protocol getting involved. Peers with `rtt_to == None` (no recent ping measurement) rank at `Duration::MAX` rather than getting excluded — health already filtered the candidate set, and the NodeId tiebreaker keeps the outcome deterministic among any equally-unmeasured peers.

### Pull-based catch-up

Replicas drive `SYNC_REQUEST(since_seq=local_next, chunk_max=N)` on every tick where `is_leader_silent == false && believed_leader.is_some() && local_next < leader_tail_seq`. The leader's `handle_sync_request` reads `[since_seq, since_seq+chunk_max)` from its local file, packs into a `SYNC_RESPONSE`, and ships. The replica's `apply_sync_response` validates strict monotonicity (`prev.checked_add(1)`), enforces a 64 MiB hard chunk ceiling even for the "admit at least one event" branch (so an oversize first event NACKs back rather than DOSing the wire), and routes typed `RedexError` variants (`DiskPressure`, `Closed`) to the right runtime handler.

### Bandwidth budgets

`BandwidthBudget` is a token bucket sized at `replication_budget_fraction × measured_NIC_peak`. The catch-up loop calls `try_consume(estimated_bytes, now)` before shipping each chunk; full bucket admits; partial defers and NACKs back `Backpressure`. Oversize requests (a single event larger than one-second's capacity — rare but representable) admit as a one-off and drain the bucket fully, so the channel can never deadlock trying to ship an event it can never afford.

### Heartbeats + repair

`HeartbeatTracker` per channel per node holds `(last_seen, role, tail_seq)` for every peer. The runtime tick emits a heartbeat to every non-self peer in the replica set when role ∈ {Leader, Replica}; inbound heartbeats update the tracker and refresh the `believed_leader` cell. `is_leader_silent` trips when `now - last_seen > heartbeat_ms × miss_threshold` (default 3× = 1.5 s at the 500 ms heartbeat), triggering the `Replica → Candidate` transition and the in-tick election. `heartbeat_ms` is now validated to `[100, 300_000]` so a unit-confused config (μs instead of ms) can't saturate the silence-detection multiplication and silently disable failover.

### Failover + replica rejoin

Plan §7: leader loss → silence detection → Candidate → election → Leader/Replica per `ElectionOutcome`. Plan §8: a replica rejoining from a longer-than-trim outage observes `first_seq > local_next` on the next `SYNC_RESPONSE`; if `first_seq == leader_first_retained_seq` the gap is a legitimate retention trim and `RedexFile::skip_to(first_seq)` runs (bumping `dataforts_replication_skip_ahead_total`), any other shape is treated as divergence and NACKs back.

### Cross-binding API surface

Every binding ships the same two-method extension to its existing `Redex` type:

- `enable_replication(mesh)` — installs the `SUBPROTOCOL_REDEX` inbound router on the mesh and arms `Redex::open_file` to spawn a replication runtime when the supplied `RedexFileConfig` carries `replication: Some(ReplicationConfig)`. Idempotent: a second call with the same mesh is a no-op.
- `open_file(name, cfg)` — when `cfg.replication.is_some()`, spawns a per-channel `ReplicationRuntime` (tokio task + `HeartbeatTracker` + `BandwidthBudget` + `ReplicationCoordinator`) and registers it on the inbound router. Reopen with a structurally-different `ReplicationConfig` returns a typed error rather than silently reusing the original.

The substrate exposes `Redex::replication_runtime_count()`, `Redex::replication_coordinator_for(name)`, `Redex::replication_status_snapshot()`, and `Redex::replication_metrics_snapshot()`. The metrics snapshot is also rendered to Prometheus text via `Redex::replication_prometheus_text()` for direct scraping.

### Metrics

Per-channel atomic counters (`ChannelMetricsAtomic`) — `sync_bytes_total`, `sync_request_total`, `sync_response_total`, `sync_nack_total`, `leader_changes_total`, `election_thrash_total`, `under_capacity_total`, `skip_ahead_total`, `applied_events_total`, `applied_bytes_total`, `leader_lag_micros`, `replica_lag_micros`. Gauges (`leader_lag_micros`, `replica_lag_micros`) saturate one tick below `LAG_NOT_OBSERVED = u64::MAX` so a follow-up arithmetic operation can't accidentally collide with the sentinel. The Prometheus registry caps at 4096 channels to bound a hostile multi-channel scrape; entries past the cap are silently dropped at insertion.

### Observability + operator ergonomics

`Redex::replication_status_snapshot()` returns a `Vec<ChannelReplicationStatus>` with `channel`, `role`, `replica_set`, `believed_leader`, `tail_seq`, `lag_micros`, `under_capacity_total` per channel. Plug into a Prometheus exporter via the `replication_prometheus_text()` text-format helper; pipe into a Grafana dashboard via the per-channel labels.

---

## RedEX Distributed test strategy

The plan's test matrix landed in full:

- **Unit** — pure-function coverage for `replication_state`, `replication_election`, `replication_heartbeat`, `BandwidthBudget`, `replication_metrics`, the wire codec, and `replication_catchup`. Every pre-fix correctness item from the second-pass review ships with at least one regression test.
- **Integration (e2e)** — multi-tokio-thread tests under `tests/redex_replication_e2e.rs` covering two-node catch-up, leader-close → replica election, three-node fanout, lag-driven catch-up, heartbeat round-trip, and the `bandwidth_budget_metric_field_is_plumbed` smoke. The `replication_overhead_within_30_percent_budget` perf-budget test is marked `#[ignore]` and lives off CI's default matrix — wall-clock perf on shared CI runners isn't a stable signal.
- **DST (deterministic-simulation)** — 14 scenarios under `tests/redex_replication_dst.rs` covering happy-path catch-up, isolated-replica no-advance, partition heal, asymmetric / symmetric failover, three-node central-peer convergence, restart-during-sync, divergence-freedom after partition-heal AND after kill-revive (the original C-2 single-path scenario expanded), election storms (the C-1 scenario; storm rounds now assert `election_thrash_total` bumps), and `wall_clock_ms` determinism. The harness derives wall-clock time from a step counter, not real `Instant::now`, so traces reproduce byte-identically across machines.
- **Loom** — atomic-pattern models for `RedexFile::close`'s swap-true-on-close, the `record_tail_seq` CAS loop, the replication metrics counters under concurrent increment (including a three-way same-counter contention case), and the `try_first_close` first-call-wins flag.

---

## Hardening — `redex-distributed` second-pass review

A two-pass review of the replication branch (`docs/misc/CODE_REVIEW_2026_05_11_REDEX_DISTRIBUTED.md`) landed sixty-four numbered items (R-1..R-64) plus four coverage gaps (C-1..C-4). The first pass closed forty-four; the second-pass review on 2026-05-12 surfaced one regression in the original R-23 fix plus nineteen new items; all closed before the v0.14 branch cut. Grouped by area:

### Runtime / coordinator correctness

- **Role-flip TOCTOU closed.** `SyncRequest` and `SyncResponse` handlers re-check `coordinator.role()` immediately before the dispatcher send so a concurrent transition between the entry check and the outbound ship triggers a clean NACK `NotLeader` rather than a response from a node that no longer claims leadership.
- **Chain-tag side-effects serialized.** The coordinator's `transition_to` holds a `tokio::sync::Mutex<()>` across the state update + metric bumps + sink call so two racing transitions can't interleave `announce_chain` from a stale role over a `withdraw_chain` from a fresher one.
- **NACK `NotLeader` / `BadRange` actually recover.** `NotLeader` clears the believed leader so the next tick re-resolves via `find_chain_holders`; `BadRange` calls `RedexFile::skip_to(since_seq + 1)` and re-issues the request, rather than logging-and-dropping.
- **Post-election failure no longer strands Candidate.** When the second `transition_to` (Candidate → Leader / Candidate → Replica) surfaces `TagSink` (state moved, side-effect failed) or `Transition` (state moved out from under us), both error branches clear the believed leader so the next tick re-enters discovery from a clean slate.
- **Disk-pressure / channel-closed pick the valid signal per current role.** The transition matrix only permits `DiskPressureWithdraw` on `Replica → Idle`; Leader / Candidate variants now route through `ChannelClose` (the universal escape) so a Leader observing disk pressure actually withdraws rather than logging the matrix-reject and continuing to write through.
- **`cancel()` can't hang.** Uses `try_send(Shutdown)` first; on `Full`, aborts the `JoinHandle` directly so a wedged task with a saturated inbox can't block the caller waiting on a buffer the task may never drain.
- **`Drop` on `ReplicationRuntimeHandle` aborts the task.** The strong-reference cycle `MeshNode → router → handle → task → dispatcher Arc` is broken unconditionally when the handle goes out of scope, not just via the canonical `ReplicationWiring::drop` un-installation.
- **`is_stopped` consults an explicit flag flipped after `cancel()`'s `.await` returns**, not the `JoinHandle` slot — two concurrent `cancel()`s racing on `task.lock().take()` could previously let the loser observe `None` and report `stopped == true` before the winner had finished joining.
- **Channel-id validation defense-in-depth on every inbound type.** `SyncRequest`, `SyncResponse`, `SyncNack`, `Heartbeat` all gate on `msg.channel_id == inputs.channel_id` at the runtime boundary so mesh misroute can't poison the tracker.
- **`GapBeforeChunk` underflow closed.** `first_seq.saturating_sub(local_next)` plus a `debug_assert!(first_seq > local_next)` belt-and-suspenders.

### Catch-up correctness

- **Retention-trim vs. divergence disambiguation.** `SyncResponse` carries `leader_first_retained_seq` on the wire; the replica treats `first_seq == leader_first_retained_seq` as a legitimate trim (skip-ahead via `RedexFile::skip_to`) and any other gap shape as divergence (NACK back, bump counter, log loudly).
- **Empty chunk validates `first_seq`.** The short-circuit on `response.events.is_empty()` now validates `first_seq >= local_next` so a leader bug emitting `first_seq = 999` on an empty chunk isn't silently accepted.
- **64 MiB hard ceiling enforced for oversize first event.** The "admit at least one event" branch rejects events larger than `CHUNK_MAX_HARD_CEILING_BYTES` rather than shipping wire bytes that the replica's local append would refuse.
- **`prev + 1` strict-monotonicity uses `checked_add`.** Practically unreachable; surrounding code used `saturating_*` and the asymmetry was the real bug.
- **Lag-driven `SyncRequest` filters `believed_leader != self`** so a test-setup loopback or tracker misuse can't make the runtime issue a `SyncRequest` to itself.

### Wire codec

- **`SyncNack::from_bytes` truncation reports correct `need`.** The R-23 fix shipped for `SyncResponse` but missed the `SyncNack` arm in the original commit; the second-pass review caught and fixed it.
- **`SyncNack::to_bytes` truncates at a UTF-8 char boundary.** A multi-byte codepoint straddling `SYNC_NACK_DETAIL_MAX` previously shipped invalid UTF-8 that the decoder rejected, losing the structured error code along with the diagnostic.
- **`WireError::Truncated.need` formula correct everywhere.** `need = consumed + still_needed` in every arm — both header reads and per-event payload reads.

### File / manager

- **`RedexFile::skip_to` panic-safe swap order.** Builds the new index / timestamps into temp `Vec`s, calls `evict_prefix_to` against the segment, then assigns the new index. Pre-fix a panic between the index swap and the eviction call would leave the index referencing pre-eviction offsets.
- **Reopen with differing replication config rejects with a typed error** rather than silently reusing the original. Compares against the live coordinator's config; accepts `None ↔ None` and `Some(cfg_a) ↔ Some(cfg_b)` where the two are structurally `PartialEq`, rejects everything else.
- **`mod replication` dual public surface collapsed.** The flat re-exports under `redex::` are now the only public path; `pub mod replication` is gone.
- **Lag saturation pinned with a named constant** `LAG_SATURATED_MICROS = LAG_NOT_OBSERVED - 1` and a test asserting the gap from the sentinel is preserved.

### Mesh / dispatch

- **`from_node == 0` sentinel collision rejected.** The replication inbound arm mirrors the reflex handler's guard — a peer whose `from_node` falls back to `0` (the valid `NodeId` sentinel collision) is dropped rather than entering the tracker.

### Bindings / FFI

- **Python `replication=False` with `replication_*` kwargs rejects with a typed `RedexError`** rather than silently dropping the other kwargs.
- **`enable_replication` is a typed `RedexError` stub without the `net` feature** in both Node and Python, rather than `TypeError: redex.enableReplication is not a function` / `AttributeError`. The Python `replication_runtime_count` / `replication_prometheus_text` gates the same way.
- **`net_redex_enable_replication` drops `Box<Arc<MeshNode>>` on every error path.** Doc-comment now states "consumed regardless of return code."
- **`net_redex_open_file` and `net_redex_file_tail` pre-zero `*out_handle` / `*out_cursor` on entry** so a cgo / C consumer reading the slot after a non-zero return sees null rather than stale stack data.
- **Python `runtime.block_on` paths release the GIL via `py.detach`** across the blocking open / open-from-snapshot / tail / watch / snapshot-and-watch paths. Existing precedent (`wait_for_seq`, `__next__`) already did this; the cortex open / tail / watch paths now match.
- **Node `RedexFile.sync` and `RedexFile.close` are async** — disk I/O dispatches via `tokio::task::spawn_blocking` onto the napi worker pool instead of running on the JS event-loop thread. The other read-side methods stay sync (in-memory only).
- **Python rejects kebab-case spellings** for `colocation_strict` / `evict_oldest`; Node rejects snake_case for the same (each binding accepts only its idiomatic spelling). The FFI core remains liberal so the Go-facing JSON shape can use either.
- **`Pinned([])` rejected at the binding layer** with a typed error rather than falling through to the core validator.
- **`leader_pinned` cross-checked against `pinned_nodes` at the binding layer** when `placement == Pinned`.
- **Node `redex_err` documents the `redex:` prefix contract** in `index.d.ts` so JS-side operators can string-sniff on `e.message.startsWith("redex:")` against a pinned shape.
- **Go `OpenFile` distinguishes `ErrInvalidReplicationConfig` from `ErrReplicationRequiresEnable`.** Binding-side validator covers shape errors plus `Factor` / `HeartbeatMs` ranges; only the FFI `NET_ERR_REDEX` for replication-not-enabled falls into the second sentinel.
- **Go `RedexFile.mu` uses `sync.RWMutex`** so appends / reads aren't serialized per file. The Rust substrate's `HandleGuard` is a reader-counter; pre-fix the Go binding's mutex defeated that.
- **Go `typedef ArcMeshNode` aliases the upstream `net_compute_mesh_arc_t`** opaque typedef so the same Arc<MeshNode> handle works through both surfaces.

### Hygiene + coverage

- **Election sort uses `sort_unstable_by`.** The total compound key `(rtt, node_id)` provides determinism; stability isn't load-bearing.
- **Event-vec preallocation cap (4096) documented** in the wire codec.
- **`u32::try_from(payload_len).unwrap_or(u32::MAX)` carries a `debug_assert!`** so accidental misuse surfaces in debug builds rather than silently corrupting on the wire.
- **DST harness `wall_clock_ms` derives from the step counter**, not real `Instant::now`. Traces reproduce byte-identically across machines.
- **DST election-storm scenario asserts `election_thrash_total`.** The harness mirrors the production coordinator's counter locally so storm rounds can observe the gauge without rewiring the harness around the async coordinator.
- **Divergence-freedom check runs after `partition_heal` AND after `restart_during_sync`**, not just on the happy path.
- **e2e flake-prone test marked `#[ignore]`.** The `replication_overhead_within_30_percent_budget` 1.3× wall-clock budget is opt-in via `cargo test -- --ignored` rather than running on shared CI runners.
- **e2e `bandwidth_budget_is_observable_in_metrics` renamed to `bandwidth_budget_metric_field_is_plumbed`** so the test name matches what the test asserts (field plumbing under the wire path, not budget engagement; the budget-fired path is unit-tested under `replication_catchup`).
- **Loom metrics model exercises a three-way same-counter contention case** beyond the existing two-thread mixed-counter races.
- **`BandwidthBudget::try_consume` handles oversize requests** via the full-bucket admit-once-and-drain escape hatch so a single event larger than one-second's capacity can't deadlock the channel.
- **Election ranks unmeasured-but-healthy peers at `Duration::MAX`** rather than excluding them — health already filtered the candidate set, and the NodeId tiebreaker keeps the outcome deterministic among any equally-unmeasured peers.

### CI

- **Three new CI jobs.** `redex-replication-e2e` runs the multi-tokio-thread integration suite under `--features "redex net"`; `redex-replication-dst` runs the deterministic-simulation harness under `--features redex`; `loom-models` runs the atomic-pattern loom tests under `RUSTFLAGS=--cfg loom`. All three gate the `redex-distributed` merge.

---

## Capability hardware units — MB → GB / Mbps → Gbps

**v0.14 changes the hardware-axis numeric units from megabyte / megabit-per-second to gigabyte / gigabit-per-second across core and every binding.** The tag keys, predicate builders, FFI shapes, and JSON schemas all rename. This is a breaking wire-format change for any `CapabilitySet` that carries hardware numerics.

The motivation is operator ergonomics — fleets in 2026 routinely advertise hundreds of GB of memory and tens of Gbps of network capacity, and the MB / Mbps wire shape forced operators to read values like `65_536` and `10_000` when `64` / `10` is what they meant. The smaller numeric range also fits cleanly in `u32` for the wire encoding.

### Tag / key renames

| Old (v0.13) | New (v0.14) |
|-------------|-------------|
| `hardware.memory_mb` | `hardware.memory_gb` |
| `hardware.gpu.vram_mb` | `hardware.gpu.vram_gb` |
| `hardware.storage_mb` | `hardware.storage_gb` |
| `hardware.network_mbps` | `hardware.network_gbps` |
| `hardware.accelerator.<i>.memory_mb` | `hardware.accelerator.<i>.memory_gb` |

Adjust values when migrating: `65_536 MB` → `64 GB`, `81_920 MB` → `80 GB`, `10_000 Mbps` → `10 Gbps`.

### Filter / predicate renames

| Old | New |
|-----|-----|
| `min_memory_mb` | `min_memory_gb` |
| `min_vram_mb` | `min_vram_gb` |
| `min_storage_mb` | `min_storage_gb` |
| `min_network_mbps` | `min_network_gbps` |

The predicate builders (`p.minMemory(...)`, `p.minVram(...)`, etc. in TS; the `p.min_memory(...)` family in Python; `Predicate{}.MinMemory(...)` in Go) now produce `NumericAtLeast` tags whose key is `memory_gb` / `vram_gb` / `storage_gb` / `network_gbps`.

### Binding surfaces

| Binding | Renamed fields / keys |
|---------|----------------------|
| **Rust core** | `HardwareCapabilities::memory_gb`, `GpuCapability::vram_gb`, `HardwareCapabilities::storage_gb`, `HardwareCapabilities::network_gbps`, `AcceleratorCapability::memory_gb`. `Capabilities::with_memory(gb)` takes GB; `ResourceEnvelope::max_memory_gb`, `ResourceClaim::memory_gb`, `TopologyHint::{uplink_gbps, downlink_gbps}` all moved to GB / Gbps. |
| **Go** | `HardwareCaps.MemoryGB`, `GPUInfo.VRAMGB`, `HardwareCaps.StorageGB`, `HardwareCaps.NetworkGbps`, `AcceleratorInfo.MemoryGB`. |
| **Node** | `Hardware.memoryGb`, `Hardware.storageGb`, `Hardware.networkGbps`, `GpuInfo.vramGb`, `AcceleratorJs.memoryGb` (all `index.d.ts`). |
| **Python** | dict keys `memory_gb` / `vram_gb` / `storage_gb` / `network_gbps`; accelerator dict key `memory_gb`. Stubs (`net_sdk.*.pyi`) and tests updated. |
| **C / FFI** | Capability / filter JSON uses `*_gb` keys (`min_memory_gb`, `min_vram_gb`, `min_storage_gb`) and `network_gbps`. |

### Refactors

The core schema (`AXIS_SCHEMA`) and tag codec emit / parse the new `*_gb` / `*_gbps` keys. Placement / scoring and proximity tiers use a 16 GB baseline (was 16 GB previously; the renames are nominal, not behavioral). Serialization APIs that took MB-shaped values now take GB. Safety types and topology hints align. Docs, benches, examples, and every test fixture / cross-binding golden vector regenerate against the new shape; the final sweep removed lingering `network_mbps` references across `tests/cross_lang_capability/` and the per-binding compat suites.

### Cross-binding fixtures

The thirteen fixtures under `tests/cross_lang_capability/` regenerate against the new unit. `predicate_eval`, `capability_set_diff`, `capability_validation`, `placement_score`, and the numeric-parity fixtures all carry GB / Gbps values. `predicate_nrpc_envelope.json` bumps `abi_version_expected: 1 → 2` (see below).

---

## Predicate-on-the-wire header — `cyberdeck-where:` → `net-where:`

The HTTP / nRPC header carrying predicates from caller to callee was named `cyberdeck-where:` in v0.13 — the project umbrella on the wire. v0.14 renames to `net-where:` for three reasons:

1. **HTTP / nRPC convention names the protocol, not the parent organization.** HTTP doesn't have `w3c-content-type:`; `traceparent` / `idempotency-key` use system-level prefixes, not org names. The umbrella-on-the-wire shape was an outlier.
2. **The header is not nRPC-specific even though it currently rides nRPC.** Predicates are protocol-agnostic; any future predicate-bearing surface (raw channel pre-filter, subprotocol call hook, …) should ride the same name. `net-where:` brackets the right layer (the net crate / SDK), not a specific service inside it.
3. **Symmetric naming with the substrate crate.** Net's other reserved headers and protocol identifiers carry the `net-` / `net_` prefix; lining this one up makes the surface easier to grep and easier to teach.

### `RPC_WHERE_HEADER` constant

Every binding exports the new name as a pinned constant:

- Rust: `net::adapter::net::behavior::predicate::RPC_WHERE_HEADER = "net-where"`
- TS: `import { RPC_WHERE_HEADER } from '@ai2070/net-sdk'`
- Python: `from net_sdk import RPC_WHERE_HEADER`
- Go: `net.RPCWhereHeader`
- C: `NET_PREDICATE_WHERE_HEADER` macro in `net.go.h`

Server-side decoders accepting the v0.13 `cyberdeck-where:` name are not provided. Mixed v0.13 / v0.14 fleets cannot exchange predicates over the wire; recommend lockstep upgrade alongside the capability-unit migration.

### Predicate envelope ABI version bump

`tests/cross_lang_capability/predicate_nrpc_envelope.json` bumps `abi_version_expected: 1 → 2` to signal the wire-format change. No binding-side ABI version constants pin to 1 — none of the per-binding tests asserted on the envelope fixture's version — so the bump is informational + future-defensive. Future header / envelope changes in v0.15+ will bump to 3 against the same fixture.

---

## Test hygiene

- **Cross-binding wire-format fixtures regenerate against the new units + header name.** Thirteen fixtures under `tests/cross_lang_capability/`, all versioned via `abi_version_expected: 2` for the predicate envelope (other fixtures continue at `1` — only the envelope carries the ABI version field today).
- **Three new CI jobs.** `redex-replication-e2e`, `redex-replication-dst`, `loom-models` gate the merge.
- **Lib suite at 2640+ tests** (was 2330+ at v0.13 release). 300+ net new tests across the replication + regression paths; every numbered review item ships with at least one regression where the shape made one possible.
- **`cargo clippy --all-features --all-targets -D warnings` clean** across substrate + every binding crate.
- **`cargo doc --all-features --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`** — both `rustdoc::broken_intra_doc_links` and `rustdoc::private_intra_doc_links` enforce.

---

## Breaking changes

### Wire format — `SUBPROTOCOL_REDEX` is new

`SUBPROTOCOL_REDEX = 0x0E00` is a new mesh subprotocol family; v0.13 nodes don't speak it. Mixed v0.13 / v0.14 fleets cannot exchange replication traffic. Channels opened with `replication: None` continue to work cross-version (same single-node behavior as v0.13).

### Wire format — capability hardware units

**v0.14 breaks wire compatibility with v0.13 for `CapabilityAnnouncement` / `CapabilityDiff` carrying hardware numerics.** `hardware.memory_mb` / `hardware.gpu.vram_mb` / `hardware.storage_mb` / `hardware.network_mbps` / `hardware.accelerator.<i>.memory_mb` rename to the `*_gb` / `*_gbps` shape. v0.13 receivers parse v0.14 announcements as `Tag::Legacy` (unknown axis-prefixed tags pass through under the forward-compat rule) — the values survive the round-trip but no longer satisfy `min_memory_mb` / etc. filters, so placement decisions on a v0.13 receiver may produce different verdicts. Recommend lockstep upgrade.

### Wire format — `cyberdeck-where:` → `net-where:`

**v0.14 renames the predicate-on-the-wire HTTP header.** v0.13 servers expecting `cyberdeck-where:` won't see v0.14 callers' header values; v0.13 callers' `cyberdeck-where:` won't be read by v0.14 servers. Mixed fleets must either upgrade lockstep or maintain a transitional gateway that rewrites the header on the way through.

### Rust core (`net` crate) — API surface

- **`Capabilities::with_memory(value)` takes GB**, not MB. Same for the resource-envelope / claim / topology types: `ResourceEnvelope::max_memory_gb`, `ResourceClaim::memory_gb`, `TopologyHint::{uplink_gbps, downlink_gbps}`.
- **`HardwareCapabilities` field renames** — `memory_gb`, `gpu.vram_gb`, `storage_gb`, `network_gbps`. `AcceleratorCapability::memory_gb`.
- **`adapter::net::redex` exports** — new types `ReplicationConfig`, `PlacementStrategy`, `UnderCapacity`, `ReplicationCoordinator`, `ReplicationCoordinator::transition_to`, `ReplicaRole`, `TransitionSignal`, `StateTransition`, `HeartbeatTracker`, `PeerState`, `BandwidthBudget`, `ReplicationMetricsRegistry`, `ChannelMetricsAtomic`, `ChainTagSink`, `ChannelIdentity`, `CoordinatorError`, `elect`, `ElectionOutcome`, `ChannelReplicationStatus`. The wire codec types (`SyncRequest`, `SyncResponse`, `SyncHeartbeat`, `SyncNack`, `SyncNackError`, `SyncEvent`, `WireError`, `SUBPROTOCOL_REDEX`, `DISPATCH_SYNC_*`, `SYNC_NACK_DETAIL_MAX`) re-export at the redex module root.
- **`Redex::enable_replication(mesh)` is a new method.** Idempotent; pair with `Redex::open_file` carrying `cfg.replication = Some(rep)` to spawn a per-channel replication runtime.
- **`Redex::open_file` rejects reopen with a structurally-different `ReplicationConfig`** with a typed `RedexError::Channel`. Reopen with the same config returns the existing handle (unchanged from v0.13).
- **`RPC_WHERE_HEADER = "net-where"`** (was `"cyberdeck-where"` in v0.13).
- **`HEARTBEAT_MS_MAX = 300_000`** added; `ReplicationConfig::validate` rejects `heartbeat_ms > HEARTBEAT_MS_MAX` with a typed `HeartbeatTooHigh` variant.

### Rust SDK (`net-sdk`)

- **`net_sdk::capabilities::redex` re-exports** the substrate replication surface — `ReplicationConfig`, `PlacementStrategy`, `UnderCapacity`, `ReplicaRole`, `ChannelReplicationStatus`.
- **`net_sdk::capabilities::predicate::RPC_WHERE_HEADER`** is the renamed constant.

### FFI / bindings

| Binding | Change |
|---------|--------|
| **All** | New `enable_replication(mesh)` method on `Redex`. New `replication` field on `RedexFileConfig`; pair with `ReplicationConfig` constructor. New `ReplicaRole` / `PlacementStrategy` / `UnderCapacity` enums and `ReplicationConfig` builder per binding. New `replication_runtime_count`, `replication_status_snapshot`, `replication_metrics_snapshot`, `replication_prometheus_text` getters on `Redex`. |
| **All** | Hardware-numeric field renames — `memoryGb` / `vramGb` / `storageGb` / `networkGbps` etc. per binding's idiomatic naming. Same for the predicate min-builder family — `minMemory` / `minVram` / `minStorage` / `minNetwork` now produce GB / Gbps tags. |
| **All** | `RPC_WHERE_HEADER` constant renames to `"net-where"`. Header-bearing nRPC call variants (`net_rpc_call_with_headers` etc.) pass the new name; v0.13 servers expecting `cyberdeck-where:` won't decode v0.14 callers. |
| **Node** | New `Redex.enableReplication(mesh)` method. New `replication: ReplicationConfig` field on `RedexFileConfig`. `RedexFile.sync()` / `RedexFile.close()` are async (return `Promise<void>`); callers must `await`. Pre-v0.14 code calling `file.sync()` / `file.close()` synchronously generates an orphan Promise warning under modern Node. The `redex:` JS-error prefix is pinned in `index.d.ts` doc-comment as the stable contract. |
| **Python** | New `Redex.enable_replication(mesh)` method. New `replication=` kwarg on `Redex.open_file`. `replication=False` with any `replication_*` kwarg now raises `RedexError` rather than silently dropping the kwarg. `cortex` open / tail / watch paths release the GIL via `py.detach` across the blocking work. `enable_replication` / `replication_runtime_count` / `replication_prometheus_text` are typed `RedexError` stubs without the `net` feature. |
| **Go** | New `RedexManager.EnableReplication(meshArc)` method. New `RedexFileConfig.Replication *ReplicationConfig` field. `RedexFile` uses `sync.RWMutex` so appends / reads don't serialize. `OpenFile` returns the matching sentinel (`ErrInvalidReplicationConfig` vs `ErrReplicationRequiresEnable`) per error class. `ArcMeshNode` typedef aliases the upstream `net_compute_mesh_arc_t`. |
| **C** | New entry points: `net_redex_enable_replication(redex, mesh_arc)`, `net_redex_replication_runtime_count(redex)`, `net_redex_replication_prometheus_text(redex)`, `net_free_string(ptr)`. `net_redex_open_file` / `net_redex_file_tail` pre-zero `*out_handle` / `*out_cursor` on entry. The replication config rides the `RedexFileConfigJson.replication` field; binding-side validators or the FFI core enforce numeric ranges. |

### Behavioral fixes that may surface as test breakage

- **`ReplicationConfig::heartbeat_ms` clamps at `[100, 300_000]`.** Tests injecting `u64::MAX` or other pathological values to observe silence-detection behavior will see `ReplicationConfigError::HeartbeatTooHigh` instead.
- **`PlacementFilter` election no longer excludes peers with `rtt_to == None`.** Tests that asserted `NoEligibleReplica` against an all-unmeasured replica set will see the smallest-NodeId healthy peer elected instead.
- **`SyncNack::to_bytes` truncates at a UTF-8 char boundary**, so a regression test that previously expected `from_bytes` to fail on an oversize multi-byte payload will see the round-trip succeed at a slightly-shorter detail length.
- **Reopen with a different `ReplicationConfig` rejects.** Tests that opened a channel with one config and reopened with another expecting silent reuse will see `RedexError::Channel("different from the original")`.
- **`bandwidth_budget_is_observable_in_metrics` renamed.** Tests referencing the old test name fail to find it; rename to `bandwidth_budget_metric_field_is_plumbed`.
- **`replication_overhead_within_30_percent_budget` marked `#[ignore]`.** CI runs that included this test in the default matrix will no longer see it; run via `cargo test -- --ignored`.

---

## How to upgrade

1. **Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.14 line.** Recompile / rebuild the binding cdylib (NAPI for Node, maturin for Python, `cargo build -p net-compute-ffi` + `-p net-rpc-ffi` for Go).
2. **Capability hardware-unit migration.** Rename `memory_mb` → `memory_gb`, `vram_mb` → `vram_gb`, `storage_mb` → `storage_gb`, `network_mbps` → `network_gbps`, `accelerator.memory_mb` → `accelerator.memory_gb` throughout. Adjust values: `65_536` → `64`, `81_920` → `80`, `10_000` → `10`. The predicate builders pick up the new keys automatically; tag-string literals need a manual rewrite. `cargo build` (and the binding-side TypeScript / Python static checks) drives the rewrite — the renames are compile errors.
3. **Predicate header migration.** If your call sites reference the header name directly (`"cyberdeck-where"` as a string literal), replace with `"net-where"` or use the exported `RPC_WHERE_HEADER` constant. Server-side handlers consuming the v0.13 name need the same rewrite.
4. **Replication opt-in.** Channels that want replication: call `Redex.enable_replication(mesh)` once after constructing the `Redex` (idempotent), then open each replicated channel with `Redex.open_file(name, cfg.with_replication(Some(rep_cfg)))`. The per-channel `ReplicationRuntime` spawns automatically; consult the operator surface via `Redex.replication_status_snapshot()` / `replication_prometheus_text()`.
5. **Channels that don't want replication require no changes.** Single-node channels behave identically to v0.13. `RedexFileConfig::replication = None` is the default.
6. **Node consumers — `RedexFile.sync()` / `RedexFile.close()` are async.** Add `await` to call sites:
   ```ts
   await file.sync();
   await file.close();
   ```
   Sync call sites compile but generate orphan Promise warnings under modern Node and may exit the process before the fsync lands.
7. **Python consumers — `Redex.open_file(name, replication_*=…)` requires `replication=True`.** Pre-v0.14 code passing `replication_factor=5` without `replication=True` produced a single-node channel; now raises `RedexError`. Either pass `replication=True` explicitly or drop the `replication_*` kwargs.
8. **Go consumers — `RedexFileConfig.Replication`** is the new optional field. Pass a `*ReplicationConfig` for replicated channels. Numeric validation (factor / heartbeat ranges) runs on the Go side before the FFI; structurally-invalid configs return `ErrInvalidReplicationConfig` instead of the catch-all `ErrReplicationRequiresEnable`.
9. **Fleet-wide upgrade required for any deployment using capability announcements with hardware numerics.** v0.13 receivers parse v0.14 announcements' `hardware.memory_gb` as `Tag::Legacy` — the value survives but no longer satisfies `min_memory_mb`-keyed filters. Recommend lockstep upgrade alongside the predicate-header migration.
10. **Cross-binding wire fixtures regenerated.** If you have CI that asserts golden-vector parity against `tests/cross_lang_capability/`, the GB / Gbps shape and the `net-where:` header rename mean every fixture changes. `predicate_nrpc_envelope.json` bumps `abi_version_expected: 1 → 2`; future binding-side version pins should track the per-fixture version field.
11. **Operator dashboards** — `Redex::replication_prometheus_text()` emits a per-channel snapshot in Prometheus text format; pipe into your existing scrape config under the `dataforts_replication_*` metric family. Per-channel labels (`channel`, `role`) carry the channel name and current role for dashboard slicing.
12. **DST harness integration** — if you have channel-level DST scenarios that drive `ReplicaRole` directly, the harness's `force_transition` / `tick_node` now mirror the production coordinator's `election_thrash_total` counter onto a per-`VirtualNode` `election_thrash_count` field, so storm scenarios can assert on the gauge without rewiring around the async coordinator. The harness's `wall_clock_ms` derives from a step counter, not `Instant::now`.

---

Released 2026-05-12.

## License

See [LICENSE](../../LICENSE).
