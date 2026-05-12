# Dataforts Blob Storage — Active overflow extension (v0.3 blob track, shipped in v0.15)

> Companion to [`DATAFORTS_BLOB_STORAGE_PLAN.md`](DATAFORTS_BLOB_STORAGE_PLAN.md). v0.2 mesh-native blob storage is intentionally pull-only — when a node's local disk crosses the unhealthy threshold, it advertises `dataforts:blob-storage-unhealthy` and other nodes' admission rejects inbound migrations. The local node never *pushes* its own blobs elsewhere; under sustained saturation a node either reclaims via GC or stops accepting new bytes. This document specifies the **active overflow** track that closes the loop: when a node fills up, it picks coldest blobs by inverse blob-heat and pushes them to peers that have free disk and have opted into receiving overflow.

## Status

**Shipped in v0.15 — "Rebel Yell"** (2026-05-12). P1..P5 landed on the `dataforts-overflow` branch and merge into the v0.15 release alongside the mesh-native blob v0.2 track. Feature-complete: pure-logic admission + controller + tick + wire integration + Prometheus + CLI + Python binding. Hard prerequisites all also shipped in v0.15:

- **`MeshBlobAdapter`** + chunked storage + refcount + GC (PR-5a..PR-5r).
- **`BlobHeatRegistry`** — per-chunk heat counters with half-life decay (PR-5j-a/b).
- **`CapabilityIndex` + `BlobCapability`** — disk-free / scope / health capability tags (PR-5k).
- **Gravity migration controller** — proves the pull-side decision shape; the push side is its near-mirror (PR-5j-d, PR-5o).

No backward-compat constraints: overflow is a new opt-in surface, disabled by default. Existing v0.2 deployments are unaffected. Enabling overflow on a node that hasn't enabled gravity / blob storage rejects with `OverflowReject::NoStorageCap` at admission time.

### Shipping status

| PR     | Commit       | Scope shipped                                                                                                   |
|--------|--------------|-----------------------------------------------------------------------------------------------------------------|
| P1     | `c55568a6`   | Pure-logic admission (`should_accept_overflow_from` + `OverflowVerdict` + `OverflowReject`) + `OverflowConfig` + `BlobCapability::overflow_enabled` field + `dataforts.blob.overflow` reserved tag + `MeshBlobAdapter::{with_overflow, set_overflow_enabled, overflow_enabled, overflow_config, set_overflow_config}`. 17 T-1 tests. |
| P2     | `b87f4129`   | `BlobOverflowController` + `BlobOverflowTickReport` + `step_overflow_hysteresis` + `OverflowPushSink` trait + `drive_blob_overflow_tick` + `MeshBlobAdapter.overflow_active` hysteresis state. 20 T-1/T-2 tests. |
| P3     | `8fbfa4fb`   | Wire types `OverflowPush` / `OverflowPushAck` + `OVERFLOW_PUSH_SERVICE` constant + `OverflowPushHandler` (`handle` + `RpcHandler` impl) + `MeshNode::{send_overflow_push, serve_overflow_push}` + `MeshNodeOverflowPushSink`. 7 wire + 2-node integration tests. |
| P4     | `be6448d0`   | Prometheus counter family (`dataforts_blob_overflow_*` — admitted / errors / 6-label per-reason rejected / hysteresis transitions / active gauge / disk_ratio) + `BlobMetrics::{record_overflow_tick, record_overflow_reject}` + `MeshBlobAdapter::drive_overflow_tick` convenience + `net-blob overflow status` CLI subcommand + `OverflowTickContext` / `OverflowTickObservation` arg-bundling structs. 10 T-2 tests. |
| P5     | (this PR)    | Python binding — `MeshBlobAdapter(redex, "id", overflow=...)` kwarg (bool or dict) + `.overflow_enabled` / `.overflow_active` / `.overflow_config` properties + `.set_overflow_enabled(bool)` + `.set_overflow_config(dict)` runtime setters. Typed-error path for unknown dict keys + invalid scope tokens. 12 pytest tests. |

### Still deferred — items that warrant their own design step

- **Durability watermark observation.** The plan calls for a sender-side helper (`MeshNode::wait_for_overflow_durability`) that polls the capability index for `causal:<hash>` advertisement before the sender drops its local copy. P3 + P4 ship the push side without the durability gate; the sender's "safe to delete" decision is currently implicit (rely on refcount + retention floor). A future P6 wires the explicit watermark observation + the configurable durability timeout.
- **Node + Go bindings.** Python shipped first (the project's primary FFI consumer). Node + Go follow the same per-binding cadence as the v0.2 blob track (per the parent plan's deferred-binding posture); each binding's surface is ~100 LOC + idiomatic test fixtures.
- **Operator-driven safe-delete on durability.** Once P6 lands the watermark observation, the controller's tick driver gains a `delete_after_durability` action that runs `adapter.delete_chunk(hash)` once the watermark confirms one external holder + `refcount == 0` + `!pinned`. Pre-P6 the local copy stays until the GC sweep collects it under retention.
- **Cluster-wide rebalance.** v0.3 is per-node-local — each node decides independently what to push. A coordinated cluster-rebalance (consensus on "balance is X bytes off across these N nodes") is out of scope; it's a separate control-flow shape (gossip-driven consensus vs. local decision) and the local push has shipped without demonstrated need for the global view.

## Frame

Three observations motivate active overflow over the "advertise unhealthy and stop accepting" posture:

1. **Stop-accepting fails closed for new writes; it does nothing for the bytes already on disk.** A node carrying 950 GiB of cold blobs and 50 GiB of free space has the same disk-free as a node carrying 950 GiB of hot blobs. Pure GC reclaims refcount=0 blobs but won't move *live* blobs off the node, even when a near-idle peer has 900 GiB free.
2. **Heat data already exists.** The per-chunk heat counters from PR-5j-a/b give a natural eviction-order primitive. The coldest blobs (lowest decayed rate) are by definition the cheapest to move — the local node loses something it isn't reading.
3. **Pull-only is the right *default*, not the right *only* mode.** Operators running fixed-size storage tiers benefit from a self-balancing posture; operators running ephemeral cache nodes prefer pure pull. The boolean toggle keeps both audiences first-class.

The track stays mesh-native — no new subprotocol, no new wire framing. Overflow pushes ride the existing per-chunk replication runtime: pushing node A opens the chunk channel on the receive-target B and lets the existing `SUBPROTOCOL_REDEX` replication coordinator pull the bytes. The only new bits on the wire are:

- A capability tag (`dataforts.blob.overflow`) the receive-side advertises so push-side peer selection can filter.
- A best-effort inline RPC nudge (`OverflowPush { hash, size }`) so B knows to expect inbound chunks before the replication runtime fires. The nudge is an optimization — the chunk-channel open against B is the load-bearing action.

## What ships

Six things, in dependency order:

1. **`OverflowConfig` + a single boolean toggle.** Disabled by default. `MeshBlobAdapter::with_overflow(OverflowConfig)` builder + `MeshBlobAdapter::set_overflow_enabled(bool)` runtime toggle. The boolean is the operator-facing knob; `OverflowConfig` carries the tuning knobs (high-water threshold, low-water clear threshold, max-per-tick push budget, scope).
2. **`BlobCapability::overflow_enabled` field + `dataforts.blob.overflow` reserved tag.** When set, the node advertises the tag on the next `announce_capabilities`. Peers filter overflow-target selection by this tag — a node not advertising overflow never receives a push.
3. **`should_accept_overflow_from(local_caps, sender_caps, size_bytes)` admission decision.** Pure-logic mirror of `should_migrate_blob_to` but receive-side. Inputs: local participation (`overflow_enabled` + `blob.storage` + `disk_free_gb`), sender's caps (`scope`, `overflow_enabled` — defends against single-sided pushes), and the chunk size. Verdict shape: `Admit` / `Reject(reason)` with `OverflowReject ∈ { NotParticipating, InsufficientDisk, ScopeMismatch, SenderNotOverflowing, Unhealthy }`.
4. **`BlobOverflowController` + `drive_blob_overflow_tick`.** Per-node tick loop that fires when local disk crosses the configured high-water mark. Walks the local `BlobHeatRegistry` in ascending heat order, picks N coldest hashes (bounded by `max_pushes_per_tick`), selects an overflow-enabled peer from `CapabilityIndex` with enough free disk + matching scope, and pushes. Per-reason counters mirror the migration controller's `BlobMigrationTickReport`.
5. **`OverflowPush` RPC nudge.** Best-effort `(hash, size)` notification over the existing mesh RPC surface (no new subprotocol number). The receiver opens the chunk channel with replication armed; the chunk bytes pull via the existing per-chunk replication runtime. Failure to notify is non-fatal — the receiver just doesn't pre-open the channel.
6. **Operator surface.** Prometheus metrics (`dataforts_blob_overflow_pushes_admitted_total`, `…_rejected_total{reason}`, `…_push_errors_total`, `…_high_water_triggered_total`, `…_low_water_cleared_total`, `dataforts_blob_overflow_destination_count`); `net-blob overflow status` CLI subcommand; the `dataforts.blob.overflow` cap tag is the operator-visible "this node accepts overflow" signal in `net-blob ls` / capability dashboards.

What this plan does NOT ship (explicitly deferred):

- **Inter-mesh overflow.** Subnet-to-subnet pushes compose against the existing subnet gateway machinery — out-of-scope for v0.3 unless a workload demands it. Same posture as migration in v0.2.
- **Coordinated cluster-wide rebalance.** A node decides locally what to push and where; there's no consensus on "the cluster should be balanced." Useful for steady-state but a non-goal of v0.3 — the activation gate is "a single node is full," not "the cluster is unbalanced."
- **Reverse pull-on-overflow.** A peer with free disk could observe `dataforts:blob-storage-unhealthy` on a neighbor and proactively pull blobs from it. Tractable but a different control-flow shape (subscribe to unhealthy events vs. push from full nodes). Push-side ships first; reverse-pull is a v0.4 candidate if the push side leaves coverage gaps.
- **Reed-Solomon overflow re-encoding.** Pushing a manifest hash recursively pushes its constituent chunks under the existing Replicated encoding. ReedSolomon support is out-of-scope (matches the v0.2 blob plan posture).
- **Overflow-driven gravity-tag suppression.** A node that's actively shedding cold blobs probably shouldn't simultaneously be advertising heat for those blobs. The push path passively stops bumping heat (the local fetch doesn't fire after the push completes and the local copy is GC'd); explicit suppression is an optional refinement.

---

## Design

### 1. `OverflowConfig` + boolean toggle

```rust
pub struct OverflowConfig {
    /// Operator-visible master switch. `false` by default; the
    /// `MeshBlobAdapter` never pushes, never advertises the
    /// `dataforts.blob.overflow` tag, and never accepts inbound
    /// overflow pushes when this is `false`. Toggling at
    /// runtime is supported via `set_overflow_enabled`.
    pub enabled: bool,

    /// Local disk usage at or above this ratio triggers the
    /// overflow tick. Defaults to 0.85 (lines up with the
    /// existing health-gate clear threshold so overflow fires
    /// *before* the unhealthy advertisement).
    pub high_water_ratio: f64,

    /// Local disk usage at or below this ratio clears the
    /// "actively overflowing" state. Defaults to 0.70.
    /// Hysteresis prevents flapping near the boundary.
    pub low_water_ratio: f64,

    /// Maximum number of hashes pushed per tick. Defaults to
    /// 16. Each push opens a chunk channel with replication
    /// armed, so the cap bounds the wire-side bandwidth burst.
    pub max_pushes_per_tick: usize,

    /// Topology scope bound on push-target selection. `Mesh`
    /// by default. `Zone` keeps overflow inside the zone
    /// (multi-cloud deployments configure this to keep
    /// overflow traffic off the WAN).
    pub scope: TopologyScope,

    /// Tick cadence. Defaults to 30 s. Independent of the
    /// gravity tick — overflow is push-driven by local disk
    /// state, not by inbound heat.
    pub tick_interval_ms: u64,
}

impl Default for OverflowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            high_water_ratio: 0.85,
            low_water_ratio: 0.70,
            max_pushes_per_tick: 16,
            scope: TopologyScope::Mesh,
            tick_interval_ms: 30_000,
        }
    }
}
```

The operator-facing API is the single boolean:

```rust
// Builder — typical at construction.
let adapter = MeshBlobAdapter::new("mesh-prod", redex.clone())
    .with_persistent(true)
    .with_replication(ReplicationConfig::factor(3))
    .with_overflow(OverflowConfig { enabled: true, ..Default::default() });

// Runtime toggle — flip without rebuilding the adapter.
adapter.set_overflow_enabled(true);
adapter.set_overflow_enabled(false);
```

`with_overflow(OverflowConfig { enabled: false, .. })` is the no-op default — `MeshBlobAdapter::new` already starts overflow-disabled. The builder form is for the operators who want non-default thresholds + the master switch in one call.

### 2. Capability extension

`BlobCapability` gains one field:

```rust
pub struct BlobCapability {
    pub storage: bool,
    pub disk_total_gb: u64,
    pub disk_free_gb: u64,
    pub class: BlobStorageClass,
    /// New in v0.3. `true` iff the node has overflow enabled.
    /// Push-side peer selection filters on this; receive-side
    /// `should_accept_overflow_from` rejects pushes when this
    /// is `false`.
    pub overflow_enabled: bool,
}
```

`BlobCapability::write_into(&mut CapabilitySet)` adds a `dataforts.blob.overflow` reserved tag when `overflow_enabled = true`; the tag body is empty (presence is the signal). Mirrors the shape `dataforts:blob-storage-unhealthy` already uses.

`set_overflow_enabled(bool)` triggers an `announce_capabilities` rewrite + rebroadcast so peer indices update on the next tick. The setter is cheap (atomic store + one mesh rebroadcast) so toggling under operator control is realistic.

### 3. `should_accept_overflow_from` — receive-side admission

```rust
pub fn should_accept_overflow_from(
    local_caps: &CapabilitySet,
    sender_caps: &CapabilitySet,
    size_bytes: u64,
) -> OverflowVerdict;

pub enum OverflowVerdict {
    Admit,
    Reject(OverflowReject),
}

pub enum OverflowReject {
    /// Local node doesn't carry `dataforts.blob.storage`.
    NoStorageCap,
    /// Local `dataforts.blob.overflow` not advertised.
    NotParticipating,
    /// Sender's `dataforts.blob.overflow` not advertised — guards
    /// against single-sided pushes where the sender hasn't
    /// opted in but is trying to dump bytes anyway.
    SenderNotOverflowing,
    /// Local `disk_free_gb` insufficient for the blob's
    /// `size_bytes` (rounded up — mirrors `should_migrate_blob_to`).
    InsufficientDisk,
    /// Sender's scope outside the local overflow scope boundary.
    ScopeMismatch,
    /// Local node currently `dataforts:blob-storage-unhealthy`.
    /// Refusing pushes while unhealthy prevents the failure
    /// cascade of "node A is full, pushes to B; B is also
    /// full but its overflow tag is stale; B's `disk_free` is
    /// fictional."
    Unhealthy,
}
```

Pure-logic, no I/O. Unit tests pin every reject reason against synthetic capability sets.

### 4. `BlobOverflowController` + `drive_blob_overflow_tick`

```rust
pub struct BlobOverflowController<'a> {
    pub local_caps: &'a CapabilitySet,
    pub capability_index: &'a CapabilityIndex,
    pub heat_registry: &'a BlobHeatRegistry,
    pub refcount: &'a BlobRefcountTable,
    pub config: &'a OverflowConfig,
}

impl<'a> BlobOverflowController<'a> {
    /// Compute candidate (hash, target_node_id, size) triples
    /// for this tick. Walks the heat registry in ascending
    /// `current_rate` order, takes the first
    /// `max_pushes_per_tick` unpinned hashes whose refcount is
    /// at the floor (anything we can let go of locally),
    /// and selects a target via peer-selection below.
    pub fn candidates(&self) -> Vec<OverflowCandidate>;
}

pub struct OverflowCandidate {
    pub hash: [u8; 32],
    pub size_bytes: u64,
    pub target_node_id: u64,
    pub target_caps: CapabilitySet,
    pub cold_rate: f64,
}

pub async fn drive_blob_overflow_tick<A>(
    controller: &BlobOverflowController<'_>,
    adapter: &A,
) -> BlobOverflowTickReport
where
    A: BlobAdapter + ?Sized;
```

**Trigger logic.** The tick walks `disk_free_bytes / disk_total_bytes`. If the ratio is **at or above** `high_water_ratio`, it computes candidates and pushes. Once a tick fires with overflow active, subsequent ticks continue pushing until the ratio drops **to or below** `low_water_ratio` (hysteresis identical to the health gate). Operators dashboard the `dataforts_blob_overflow_active` gauge.

**Peer selection.** For each cold hash:

1. Filter `capability_index` to peers advertising `dataforts.blob.overflow`.
2. Drop peers whose `disk_free_gb < ceil(size / 1 GiB)`.
3. Drop peers outside the configured `scope`.
4. Among survivors, pick the one with the **highest `disk_free_gb`** (greedy spread — distributes overflow across peers rather than piling on one). Ties broken by lowest `node_id` for determinism.
5. If no survivor — bump `dataforts_blob_overflow_no_target_total{hash}` and skip. The hash stays on the local node until the next tick.

**Push action.** For each `(hash, target_node_id, size)`:

1. Send a best-effort `OverflowPush { hash, size }` RPC over the existing mesh RPC surface to `target_node_id`.
2. The target's RPC handler runs `should_accept_overflow_from` and, on Admit, opens the chunk channel with replication armed against the local Redex. The chunk bytes pull from any holder advertising `causal:<chunk_hash>` via the existing per-chunk replication runtime — typically the sender, but any other holder works too.
3. The sender does NOT delete the local copy immediately. The local copy is dropped only after one of: (a) the target's `causal:<chunk_hash>` advertisement appears in the capability index (proves replication landed), or (b) a configurable durability timeout (default 60 s) elapses and `refcount == 0 && pinned == false`. Pre-fix risk: deleting before the target's advertisement appears would lose the only copy if the push failed in flight.
4. Local hash drops via the standard `delete_chunk` + refcount-entry-drop path (PR-5r hardening).

**Per-tick report.**

```rust
pub struct BlobOverflowTickReport {
    pub admitted: u64,
    pub rejected_no_target: u64,
    pub rejected_target_admission: u64,    // target ran should_accept_overflow_from and rejected
    pub rejected_send_error: u64,           // RPC send failed
    pub rejected_durability_timeout: u64,   // target never advertised causal:<hash>
    pub pushed_bytes: u64,
    pub freed_bytes: u64,                   // bytes actually reclaimed locally after target ack
    pub disk_ratio_before: f64,
    pub disk_ratio_after: f64,
}
```

### 5. `OverflowPush` RPC nudge

No new subprotocol — rides the existing mesh RPC surface (Phase 7's `SUBPROTOCOL_NRPC` from v0.13).

```rust
// Wire shape (postcard-serialized):
pub struct OverflowPush {
    pub blob_hash: [u8; 32],
    pub size_bytes: u64,
    /// Sender's canonical node_id, included so the receiver
    /// can look up the sender's caps from the capability
    /// index. Defends against a stale-cap race where the
    /// index hadn't propagated the sender's overflow tag yet.
    pub sender_node_id: u64,
}

// Wire shape of the response (best-effort; sender doesn't
// block on this).
pub enum OverflowPushAck {
    Accepted,
    Rejected(OverflowReject),
}
```

**Receive-side handler.** `MeshNode::register_rpc_inbound` dispatches `OverflowPush` to the local `MeshBlobAdapter`. The adapter runs `should_accept_overflow_from(local_caps, sender_caps_from_index, size_bytes)`, opens the chunk channel on Admit, and returns the typed verdict. Failure to receive the nudge is non-fatal — the chunk channel open by the sender is what loads bytes; the nudge is purely about pre-opening on the receive side.

**Sender-side.** Sender opens the chunk channel as a *holder* (it already has the bytes), sends the nudge, and waits for the durability watermark (target's `causal:<hash>` in the capability index OR a configurable timeout). The sender deletes the local copy on durability + heat-floor + refcount-floor; otherwise the bytes stay on disk and the next tick re-evaluates.

### 6. Integration with existing surfaces

**Greedy (Phase 1).** Cache files are *not* candidates for overflow — the greedy cache is bounded by `total_cap_bytes` and evicted by greedy's existing rules. Overflow operates on `MeshBlobAdapter`-managed CAS chunks, not greedy cache files. The two paths share a Redex but key on different file naming conventions.

**Gravity (Phase 4) — blob heat.** The eviction ordering reads `BlobHeatRegistry::current_rate(hash, now)`. A hash that's hot stays on the local node; only cold hashes are candidates. After a push completes, the sender's local blob-heat entry is dropped explicitly so a stale rate doesn't keep advertising heat for a chunk the node no longer holds (defends against the gravity migration controller pulling it back immediately).

**Migration (G-3, v0.2).** Overflow and migration can race — node A pushes hash X to B at the same moment node C admits a migration of X from A's heat advertisement. The receiver's `should_accept_*` admission is idempotent — B accepting an overflow push of X it already holds is a no-op (the chunk channel against B reuses the existing file). No coordination needed.

**RYW (Phase 5).** Untouched. RYW operates on event seq watermarks; overflow operates on chunk content addresses. The two paths share no state.

**Auth (G-6).** `OverflowPush` is unauthenticated — overflow targets a *node* not a *chain*, and the receive-side decision is gated on the overflow capability tag, not the publishing chain's ACL. A future refinement could gate overflow on a delegated `cap.blob.overflow.accept` permission token, but v0.3 ships without — the threat model is "operator-deployed peers in the same mesh," not "adversarial peers."

---

## Dataforts integration rules

One new rule extends the v0.2 integration set.

### G-7 — Overflow (Phase 3.5 / v0.3 blob track)

A node with `cap.blob.overflow = true` participates in active overflow:

1. **Push side.** When local disk usage ≥ `high_water_ratio`, the overflow tick computes cold candidates from `BlobHeatRegistry`, selects an overflow-enabled peer with free disk + matching scope, and pushes via the existing chunk-channel replication runtime.
2. **Receive side.** When an `OverflowPush { hash, size }` RPC arrives, the local node runs `should_accept_overflow_from` against the sender's capability set + the chunk size. On Admit, it opens the chunk channel with replication armed. Sender-driven, no inbound retry.

**Hard gates** (mirror G-1 / G-2 / G-3 shape):

- `cap.blob.storage = false` → never participate (push side OR receive side).
- `cap.blob.overflow = false` → never participate.
- `cap.gravity.scope` narrower than sender's scope → reject push.
- Local node `dataforts:blob-storage-unhealthy` → reject inbound push (local is already in trouble, taking on more bytes makes it worse).
- Local node `dataforts:blob-storage-unhealthy` does NOT inhibit the *outbound* push path — that's exactly when overflow needs to fire.
- `disk_free_gb` < ceil(size / 1 GiB) → reject inbound push.

**Pinning interaction.** A pinned blob is never an overflow candidate. `pin(blob_ref)` is an operator's explicit "keep this here"; overflow can't violate it. Mirrors how pin gates against `sweep_gc`.

**Refcount interaction.** A blob with `refcount > 0` is a candidate iff every reference source is a *cache* (greedy) entry, not a *fold* entry (CortEX adapter, chain refcount). Folds are sources of truth; greedy caches are speculative. Overflow can shed speculative copies but never primary references. Defended by `BlobRefcountTable::is_overflow_eligible(hash) -> bool` that walks the per-source refcount map.

Counters:

- `dataforts_blob_overflow_pushes_admitted_total` — successful pushes that pulled to durability.
- `dataforts_blob_overflow_pushes_rejected_total{reason}` — `reason ∈ OverflowReject` variants.
- `dataforts_blob_overflow_push_errors_total` — RPC send errors / chunk channel open failures on the sender side.
- `dataforts_blob_overflow_high_water_triggered_total` — count of ticks that fired (disk ≥ high_water).
- `dataforts_blob_overflow_low_water_cleared_total` — count of ticks that re-entered the inactive state.
- `dataforts_blob_overflow_destination_count` — gauge of distinct peers an overflowing node has pushed to in the rolling tick window.
- `dataforts_blob_overflow_active` — gauge: `1` while overflow is firing, `0` otherwise. Hysteresis means the gauge tracks the controller state, not the raw disk ratio.

---

## Consistency / durability semantics

### O-1 — Durability of an overflow push

A push completes (sender reclaims the local copy) only when one of:

1. The target advertises `causal:<chunk_hash>` in its capability set — observable via the local capability index. Proves the chunk channel opened, the replication coordinator pulled bytes, and the receive-side file is durable to the receiver's configured replication factor.
2. The configured durability timeout (default 60 s) elapses AND the local refcount is at the floor AND the chunk is unpinned. In this case the local node logs `dataforts_blob_overflow_durability_timeout_total{hash}` and proceeds to delete — accepting the risk that the push didn't complete and any future read will fail. Operators set the timeout based on their replication-factor tolerance.

The non-timeout path is the load-bearing case; the timeout is an escape hatch so a node under sustained pressure isn't stuck holding bytes whose target peer became unresponsive mid-push.

### O-2 — Push ordering vs. event ordering

Overflow operates on content addresses, not events. There's no event-ordering implication — a chunk channel's content is content-addressed and any holder satisfies a read. A consumer reading a `BlobRef` after the push sees the same bytes regardless of which node serves the fetch.

### O-3 — Partition semantics

Under partition, a push to a target on the other side of the partition fails as either an RPC error (no route to peer) or a durability timeout (no `causal:` advertisement reaches the sender's view). Both bump distinct counters; the next tick re-evaluates against the current capability index, which reflects the partition. Once the partition heals, the next tick can push to the previously-unreachable peer normally.

### O-4 — Sender failure mid-push

Sender crashes after sending the nudge but before observing durability. The local chunk file persists (RedEX disk-backed); on restart the node rebuilds its refcount + heat state and the next overflow tick re-evaluates. If the target had already pulled bytes, its `causal:<hash>` advertisement is visible to the recovered sender, and the chunk drops on the next eligible sweep.

---

## Test strategy

Mirrors the v0.2 test strategy (§ T-1..T-5 of `DATAFORTS_BLOB_STORAGE_PLAN.md`).

### T-1 — Unit (pure-logic)

- `should_accept_overflow_from` against synthetic capability sets — every reject variant, every admit path. ~10 cases.
- `BlobOverflowController::candidates` against synthetic `(BlobHeatRegistry, BlobRefcountTable, CapabilityIndex)` triples — coldest-first ordering, pinned-skip, fold-refcount-skip, no-target case, cap on `max_pushes_per_tick`, hysteresis. ~15 cases.

### T-2 — Integration (multi-thread tokio)

- `drive_blob_overflow_tick` end-to-end against a `MockBlobAdapter` recording push targets. Asserts: tick fires only when disk ≥ high_water; coldest hashes pushed first; per-reason counters bump correctly.
- Concurrency: 8 threads concurrently store hashes while the overflow tick runs. No torn refcount / heat-registry state.

### T-3 — DST (deterministic-simulation)

- 3-node DST: A is overflow-enabled with disk at 90 %, B is overflow-enabled with disk at 30 %, C is overflow-enabled with disk at 50 %. Drive a single tick on A. Assert: A pushes to B (highest disk_free wins); A doesn't push to C. Cap A's tick at 4 pushes; verify the 5th-coldest hash stays on A.
- 3-node DST: A overflow-enabled; B overflow-DISABLED; C overflow-enabled. A's tick can only push to C even though B has more free disk.

### T-4 — Conformance

- Adapter-trait conformance unchanged from v0.2 — overflow is internal to `MeshBlobAdapter`, not part of the `BlobAdapter` trait surface. A non-mesh adapter doesn't participate.

### T-5 — Cross-binding

- Python: pytest covering the `set_overflow_enabled` kwarg + getter, plus a 2-node toy fixture (two `MeshBlobAdapter` instances against a shared in-process mesh) round-tripping a push.
- Node + Go: same shape; deferred per-binding (consistent with v0.2's deferred-binding posture).

---

## Open design questions to lock before implementation

1. **Should overflow have a separate scope axis from migration?** Proposal: reuse `cap.gravity.scope` (overflow is a gravity-adjacent push behavior). Counter: an operator might want `gravity.scope = Mesh` (heat-driven migration is mesh-wide) but `overflow.scope = Zone` (overflow stays inside the zone for bandwidth). If a workload demands the split, ship a `cap.blob.overflow.scope` later — single scope axis ships in v0.3.
2. **Durability timeout — fixed default or per-config?** Proposal: 60 s default, operator-tunable via `OverflowConfig::durability_timeout_ms`. Tight bound (10 s) trades robustness for fast reclaim; loose bound (5 min) trades reclaim latency for fewer false losses. 60 s aligns with the replication coordinator's typical first-replica latency on mesh-local peers.
3. **Should `set_overflow_enabled(false)` cancel in-flight pushes?** Proposal: no — in-flight chunk channels remain open until the receive side completes or the durability timeout fires. The boolean gates *new* tick firings, not in-flight work. Otherwise the toggle becomes async (await every cancellation), which complicates the operator-facing API.
4. **Inverse-heat tie-breaker.** Two chunks with `current_rate = 0.0`. Proposal: order by `first_seen_ms` ascending (oldest first) — oldest cold chunks have the most accumulated GC pressure and are the cheapest to lose.
5. **Should the sender refuse to push when its `cap.blob.overflow` was just set to true and the capability index hasn't propagated yet?** Proposal: yes — sender does a `cap.blob.overflow` self-check in the index before firing the first push. Avoids the race where A flips its tag, then immediately pushes to B, but B's local view of A still shows `overflow = false`. ~1 tick of delay; acceptable.

---

## Risks

- **Push storm under coordinated high-water trigger.** All N nodes in a region hit 85 % simultaneously and all try to push to the one node with free disk. Mitigation: `max_pushes_per_tick = 16` defaults bound the burst; peer selection's "highest disk_free wins" naturally spreads across multiple targets if more than one peer is below threshold; the receive-side admission rejects when its own disk fills. Worst case is push errors + bumped counters, not corruption.
- **Hash gravity reverse.** Node A pushes cold hash X to B. Some consumer then reads X, A's `BlobAdapter::fetch` delegates to the replication runtime which pulls X back from B. Net: bytes moved twice. Mitigation: gravity migration's heat is per-node, so A's heat for X stays at 0 after the push (the local read incremented blob-heat *on B*, not A); A doesn't pull X back unless local reads start happening, which is exactly when it *should* be local.
- **Stale capability index after toggle.** Operator runs `set_overflow_enabled(true)` on A; A's neighbors don't see the tag yet; A's first tick selects no target. Mitigation: the setter rebroadcasts immediately + the tick retries every `tick_interval_ms` (30 s default). Acceptable to skip one tick.
- **Refcount double-drop on overflow + GC race.** A's overflow tick pushes X to B; the durability watermark arrives; meanwhile A's GC sweep ran with disk_pressure=true and already deleted X. Mitigation: `delete_chunk` is idempotent on the refcount table (post-PR-5r); the overflow path checks `chunk_file_exists` before scheduling a deletion. Safe.
- **Operator confusion: "is overflow on or off?"** Mitigation: `MeshBlobAdapter::overflow_enabled() -> bool` getter; `dataforts_blob_overflow_active` gauge in Prometheus; `net-blob overflow status` CLI subcommand prints both the configured boolean + the runtime active state. Three converging signals.

---

## Effort

### Planning unit P1 — `OverflowConfig` + capability tag + admission primitive (1 week)

- `BlobCapability::overflow_enabled` field + `dataforts.blob.overflow` reserved tag (`write_into` + `from_capability_set`).
- `OverflowConfig` + `Default` impl.
- `MeshBlobAdapter::{with_overflow, set_overflow_enabled, overflow_enabled}` builder + setter + getter.
- `should_accept_overflow_from` + `OverflowVerdict` + `OverflowReject`.
- T-1 unit coverage.

### Planning unit P2 — Controller + tick + peer selection (1 week)

- `BlobOverflowController` + `candidates`.
- `drive_blob_overflow_tick` + `BlobOverflowTickReport`.
- High-water / low-water hysteresis.
- T-2 integration coverage on a 2-node harness.

### Planning unit P3 — RPC nudge + receive-side handler (1 week)

- `OverflowPush` + `OverflowPushAck` wire types.
- `MeshNode::register_rpc_inbound` dispatch.
- Receive-side flow: `should_accept_overflow_from` → open chunk channel → return ack.
- Durability watermark observation (sender side) + safe-delete gating.
- T-3 DST on 3 nodes.

### Planning unit P4 — Operator surface + metrics + CLI (1 week)

- Prometheus metric family.
- `net-blob overflow status` subcommand (`--format json` parity).
- Capability dashboard surfacing of the `dataforts.blob.overflow` tag.
- T-2 / T-3 metrics-shape assertions.

### Planning unit P5 — Cross-binding + docs (1 week)

- Python binding: `MeshBlobAdapter(redex, "id", overflow=False)` kwarg + `.set_overflow_enabled(bool)` + `.overflow_enabled` property.
- Node + Go: surface-only land (the underlying Rust adapter does the work); deferred per-binding rather than bulk.
- `RELEASE_v0.16` notes.
- Update `DATAFORTS_BLOB_STORAGE_PLAN.md`'s "Storage-overflow push-to-peer" deferred item to point here.

**Total: ~5 weeks** for v0.3 single-binding (Python). Node + Go follow per the v0.2 deferred-binding cadence.

---

## SDK plan

The boolean is the load-bearing operator API; every binding's surface reflects that. Tuning knobs live on `OverflowConfig` for the typed builder path.

### Rust core

```rust
// Construction-time, with operator overrides.
let adapter = MeshBlobAdapter::new("mesh-prod", redex.clone())
    .with_replication(ReplicationConfig::factor(3))
    .with_overflow(OverflowConfig {
        enabled: true,
        high_water_ratio: 0.80,
        ..Default::default()
    });

// Runtime toggle — no rebuild.
adapter.set_overflow_enabled(true);
adapter.set_overflow_enabled(false);

// Inspection.
assert!(adapter.overflow_enabled());
let config: &OverflowConfig = adapter.overflow_config();

// Tick (manual driver, typical operator-controlled cadence).
let report: BlobOverflowTickReport = adapter.drive_overflow_tick().await;
```

`Redex::enable_blob_overflow(adapter, config)` / `Redex::disable_blob_overflow(adapter)` are the **runtime-toggle** entry points consistent with the existing `enable_greedy_dataforts` / `enable_gravity_for_greedy` shape. Internally these wrap the `set_overflow_enabled` + `set_overflow_config` setters.

### Python

`MeshBlobAdapter` gains a kwarg + getter + setter. The kwarg defaults to `False` so existing call sites are unaffected.

```python
# Construction-time, simple boolean.
adapter = MeshBlobAdapter(redex, "py-prod", overflow=True)

# Construction-time, with typed config.
adapter = MeshBlobAdapter(
    redex,
    "py-prod",
    overflow=OverflowConfig(
        enabled=True,
        high_water_ratio=0.80,
        max_pushes_per_tick=8,
    ),
)

# Runtime toggle.
adapter.set_overflow_enabled(True)
adapter.set_overflow_enabled(False)

# Inspection.
assert adapter.overflow_enabled is True
print(adapter.overflow_config)  # → OverflowConfig(enabled=True, ..)

# Tick (manual driver, useful for tests).
report = await adapter.drive_overflow_tick()
print(report.admitted, report.pushed_bytes, report.disk_ratio_after)
```

`OverflowConfig` is a Python dataclass mirroring the Rust struct; kwargs accept either a `bool` (for the simple master-switch case — sugar for `OverflowConfig(enabled=bool)`) or an `OverflowConfig`. The Python binding lands in the same wheel as `MeshBlobAdapter` behind `--features dataforts` — no separate gate.

### Node

`MeshBlobAdapter` gains a `overflow` field in the construction options + a `setOverflowEnabled(boolean)` method.

```typescript
const adapter = new MeshBlobAdapter(redex, "node-prod", {
    persistent: true,
    overflow: true,                          // simple boolean
});

// Or with typed config.
const adapter = new MeshBlobAdapter(redex, "node-prod", {
    overflow: {
        enabled: true,
        highWaterRatio: 0.80,
        maxPushesPerTick: 8,
    },
});

adapter.setOverflowEnabled(true);
adapter.setOverflowEnabled(false);

console.log(adapter.overflowEnabled);
const report = await adapter.driveOverflowTick();
```

`OverflowConfig` is a TypeScript interface; camelCase field names mirror Rust's snake_case (matches the existing greedy / gravity convention).

### Go

```go
// Construction-time, with typed config.
adapter, err := net.NewMeshBlobAdapter(redex, "go-prod",
    net.MeshBlobAdapterOpts{
        Persistent: true,
        Overflow:   &net.OverflowConfig{Enabled: true, HighWaterRatio: 0.80},
    })

// Runtime toggle.
adapter.SetOverflowEnabled(true)
adapter.SetOverflowEnabled(false)

// Inspection.
fmt.Println(adapter.OverflowEnabled())
cfg := adapter.OverflowConfig()
fmt.Printf("high_water=%v\n", cfg.HighWaterRatio)

// Tick.
ctx := context.Background()
report, err := adapter.DriveOverflowTick(ctx)
```

Go follows the v0.15 convention of typed opts struct (`MeshBlobAdapterOpts`) for construction + dedicated methods for runtime toggling. `OverflowConfig` zero value is `{Enabled: false}` (Go's default zero-init aligns with the disabled-by-default contract).

### C FFI / cgo

```c
// Master switch (the simple-boolean path).
int net_mesh_blob_adapter_set_overflow_enabled(
    net_mesh_blob_adapter_handle_t handle,
    int enabled            // 0 = off, non-zero = on
);

int net_mesh_blob_adapter_overflow_enabled(
    net_mesh_blob_adapter_handle_t handle,
    int* out_enabled        // 0 = off, 1 = on
);

// Typed config (op-tuning path).
typedef struct net_overflow_config {
    uint8_t enabled;
    double  high_water_ratio;
    double  low_water_ratio;
    uint32_t max_pushes_per_tick;
    uint32_t topology_scope;        // 0=Node, 1=Zone, 2=Region, 3=Mesh
    uint64_t tick_interval_ms;
} net_overflow_config_t;

int net_mesh_blob_adapter_set_overflow_config(
    net_mesh_blob_adapter_handle_t handle,
    const net_overflow_config_t* config
);

// Manual tick driver.
typedef struct net_overflow_tick_report {
    uint64_t admitted;
    uint64_t rejected_no_target;
    uint64_t rejected_target_admission;
    uint64_t rejected_send_error;
    uint64_t rejected_durability_timeout;
    uint64_t pushed_bytes;
    uint64_t freed_bytes;
    double   disk_ratio_before;
    double   disk_ratio_after;
} net_overflow_tick_report_t;

int net_mesh_blob_adapter_drive_overflow_tick(
    net_mesh_blob_adapter_handle_t handle,
    net_overflow_tick_report_t* out_report
);
```

Without the `dataforts` Cargo feature, every entry point returns `NET_ERR_FEATURE_NOT_BUILT` (consistent with the v0.15 stub convention). Symbols link cleanly into cgo programs regardless of build configuration.

### `net-blob` CLI

```text
net-blob overflow status
  # Prints the configured boolean, the runtime active state,
  # current disk ratio, and per-reason counters.

net-blob overflow status --format json
  # Same, JSON-encoded.

net-blob overflow tick
  # Manually drives one overflow tick. Useful for operator
  # debugging / testing — production runs the tick from the
  # operator's scheduling loop.
```

The CLI does NOT expose a `net-blob overflow enable / disable` subcommand. Toggling overflow is a node-level operational decision, not a CLI gesture — operators flip it through the same channel they configure `--persistent` or `--replication-factor` (config file, daemon API, etc.). The CLI surfaces *status*; the binding API surfaces *control*.

### SDK rollout cadence

Mirrors the v0.2 mesh-native blob rollout:

1. **Rust core + Python in the same PR** (the bulk of the work is in the Rust core; the Python binding is a thin wrapper). Lands as PR-6a.
2. **Node + Go as separate per-binding PRs** (PR-6b, PR-6c).
3. **C FFI** as PR-6d.
4. **CLI status subcommand** as PR-6e.

Each per-binding PR ships with its language-idiomatic test fixture (pytest, jest, `go test`) covering construction + toggle + getter parity.

---

## Activation gate

A workload demonstrating *systematic* per-node storage saturation in a v0.2 deployment where the v0.2 "advertise unhealthy, stop accepting, GC under pressure" posture isn't enough. Realistic triggers:

- A node tier with fixed-size disks where the working set drifts past the disk-free threshold during steady-state operation (not just transient spikes).
- A deployment where node sizes are heterogeneous and the "small" nodes saturate while "big" nodes sit at 20 %. Pull-only gravity can't balance — only push-side overflow can.
- A pilot where the operator demands "cluster-self-balances under load" as a deployment property.

If the workload lives in a deployment where every node is identically sized and the working set fits comfortably on a typical node, v0.2 is the right shape — overflow adds operational complexity without a problem to solve. The boolean defaulting to `false` keeps the operator opt-in explicit.

---

## See also

- [`DATAFORTS_BLOB_STORAGE_PLAN.md`](DATAFORTS_BLOB_STORAGE_PLAN.md) — the v0.2 mesh-native blob track; § G-3 documents the pull-only posture this plan extends.
- [`DATAFORTS_PLAN.md`](../misc/DATAFORTS_PLAN.md) — the seven-phase Dataforts roadmap; overflow rides Phase 3.5.
- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — the v0.14 replication runtime overflow pushes ride on. No replication-side changes needed for v0.3.
- [`RELEASE_v0.15_REBEL_YELL.md`](../releases/RELEASE_v0.15_REBEL_YELL.md) § Mesh-native blob storage — what shipped as v0.2 and what this plan extends.
