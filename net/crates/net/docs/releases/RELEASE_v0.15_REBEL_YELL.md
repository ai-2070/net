# Net v0.15 — "Rebel Yell"

*Named after Billy Idol's 1983 album / title track — a release that asks "more, more, more" of the substrate The Warriors laid down. v0.14 made replication the load-bearing layer underneath the channel surface. v0.15 stacks the four-phase Dataforts compositional layer on top: greedy-LRU caching pulls in-scope chains, data gravity drifts hot ones toward their readers, `BlobRef` carries content-addressed pointers without owning the bytes, and read-your-writes gives producers a session-bounded "did my write land yet?" handle. No new wire protocol — every phase composes against the existing capability index, proximity graph, and `causal:` tag layer that landed in The Warriors.*

v0.15 lands **the full Rebel Yell roadmap from [`DATAFORTS_PLAN.md`](../misc/DATAFORTS_PLAN.md)** — Phases 1, 3, 4, and 5 of the seven-phase plan ship in this release, completing Dataforts as a compositional data plane on top of the v0.14 substrate. The full surface ships across Rust core, Python, Node, Go, and C FFI, with end-to-end mesh integration; greedy and gravity are runtime-toggleable policies (operators flip them on / off live against a running mesh, no rebuild required); the single `dataforts` Cargo feature gates whether the surface compiles at all. A **mesh-native blob storage extension (Phase 3.5)** lands in the same release — `MeshBlobAdapter` implements the v0.15 `BlobAdapter` trait against the local mesh's RedEX replication layer, so a Dataforts-enabled cluster has a working content-addressed blob store the moment `Redex::enable_replication(mesh)` is called. See [`DATAFORTS_BLOB_STORAGE_PLAN.md`](../plans/DATAFORTS_BLOB_STORAGE_PLAN.md) for the design. The **v0.3 active-overflow extension** ships alongside — disabled by default, one boolean to turn on; when active, a node pushes its coldest blobs to overflow-enabled peers with free disk via a new nRPC. Design + per-PR shipping status in [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`](../plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md).

The hardening posture from the Black Diamond line continues. Two coordinated code-review passes landed before the v0.15 branch cut: the primary `dataforts-feature` review ([`docs/misc/CODE_REVIEW_2026_05_11_DATAFORTS.md`](../misc/CODE_REVIEW_2026_05_11_DATAFORTS.md)) closed 54 numbered items D-1..D-54, an independent second pass surfaced 11 N-series items, all but three (deferred with rationale) closed. Five post-merge follow-up commits on the `channel-hash-32` branch hardened the RPC-inbound dispatcher hot path and tightened collision-lookup contracts.

Alongside Dataforts, v0.15 carries one cross-cutting breaking change: **the canonical channel hash widens from `u16` to `u32` substrate-wide.** The wire `NetHeader::channel_hash` stays `u16` (the 64-byte cache-line-aligned header is full), mirroring the `origin_hash u64-canonical / u32-wire` precedent. ACL, storage, config, and RYW decisions key on the canonical 32-bit hash; the wire `u16` is a fast-path filter hint only. The `PermissionToken` wire form grows from 159 → 161 bytes.

---

## Greedy-LRU dataforts (Phase 1)

Per-node speculative caching of in-scope chains observed via the tail-subscription path. The mesh fans every event through a `GreedyObserver` hook; the runtime decides whether to admit each event into a per-channel cache file. Cold channels evict under cluster-cap pressure and withdraw their `causal:<hex>` advertisement so peers re-route to a healthy holder. Wires via `Redex::enable_greedy_dataforts(mesh, config, local_caps, intent_registry)`.

### `GreedyConfig`

```rust
pub struct GreedyConfig {
    pub scopes: Vec<ScopeLabel>,           // scope-axis admission
    pub proximity_max_rtt: Option<Duration>, // proximity-axis admission
    pub per_channel_cap_bytes: u64,        // storage-axis admission, per chain
    pub total_cap_bytes: u64,              // cluster-cap eviction trigger
    pub bandwidth_budget_fraction: f32,    // share of measured NIC peak
    pub nic_peak_bytes_per_s: Option<u64>, // operator override of probe
    pub intent_match: IntentMatchPolicy,   // capability-preference axis
    pub colocation_policy: ColocationPolicy, // colocation axis
    pub observer_inflight_cap: usize,      // tokio spawn fan-out bound
}
```

The five admission axes (scope + proximity + capability-preference + colocation + storage-cap) gate every inbound event before the bandwidth-budget gate; rejected events bump the per-reason counter rather than entering the cache file. A bandwidth-budget rejection now increments a distinct `dataforts_greedy_admit_throttled_bandwidth_total` counter (was conflated with capacity-rejects pre-fix) so operators can disambiguate "NIC saturated" from "cache full." `nic_peak_bytes_per_s` overrides the hardcoded 1 Gbps default for fleets with faster NICs.

### Admission + eviction

Inbound events flow through `GreedyRuntime::dispatch_event(channel_name, channel_hash, origin_hash, chain_caps, payload)`. Admission is a pure function — `should_admit(inputs) -> AdmissionVerdict` — that returns one of `Admit` / `RejectedByAdmission(AdmitRejectReason)`. The bandwidth-budget gate runs only after admission passes; admitted events that fail the budget gate are throttled, not silently dropped (D-2). Eviction under the cluster cap returns an `EvictionSweep { evicted: Vec<EvictedEntry> }` value; the runtime calls `sink.withdraw_chain(origin_hash)` for each evicted entry inline so peers see the capability tag drop in the same tick (D-1).

The cache-side `RedexFile` keys on a synthesized `ChannelName` (`dataforts/greedy/<hex16>`) derived from the wire `u16` channel hash; the canonical 32-bit hash decision happens at the ACL / config / RYW layer, not at the data-plane cache (the wire hash is what inbound packets carry). Two channels colliding on the wire `u16` share a cache file — a small mix-up at the data-plane layer; ACL and storage decisions stay collision-safe via the canonical hash.

### TOCTOU + lock-coalescing fixes

`is_new_channel = !cache.contains(channel)` followed by `cache.upsert(...)` previously took two independent lock acquisitions; concurrent dispatch_event calls on the same channel both observed `is_new_channel = true`, both ran `sink.announce_chain`, and the second `upsert` orphaned the first `RedexFile`. v0.15 folds the lazy-open into a single `cache.get_or_insert_with` scope holding the lock for contains / open / upsert; announce fires after lock release. The steady-state path takes one lock; the new-channel path takes two with TOCTOU re-check (D-6, D-28).

`upsert` on an already-registered channel previously refreshed the file pointer without subtracting the prior entry's bytes from `total_bytes`. Reopens via `dispatch_event`'s `is_new_channel` path accumulated `total_bytes` that no eviction could ever drain, eventually starving the cluster-cap budget. The update branch now subtracts the prior `bytes` before replacing the entry's file (D-3).

`entry.bytes` saturates on overflow but didn't reflect retention trim from `RedexFile`. v0.15 ships `RedexFile::retained_bytes` + `GreedyRuntime::resync_cache_bytes` for periodic operator-driven re-anchoring; wiring is opt-in via the operator's tick loop (D-26).

### `colocation_target_held` resolved from cache

`ColocationPolicy::SoftPreference` / `StrictRequired` evaluates whether the local cache already holds chains colocated with the inbound event. The pre-fix `colocation_target_held = None` hardcode caused `StrictRequired` to reject events whose colocation target was actually present locally. The runtime now resolves the colocation target by name against the cache map (D-8).

### Spawn fan-out bound

`observe_event` is the mesh hot-path entry; without a bound, a flooding peer could create one outstanding tokio task per event before the per-event admission lock serialized them, piling up per-task `Bytes` + `Arc<CapabilitySet>` clones. v0.15 ships `observer_inflight: Arc<tokio::sync::Semaphore>` sized via `GreedyConfig::observer_inflight_cap` (default 4096); on saturation, events drop and bump `dataforts_greedy_observer_dropped_overloaded` rather than blocking the mesh dispatch task (D-7).

### Cross-binding API surface

Every binding exposes the same `enable_greedy_dataforts` / `disable_greedy_dataforts` pair plus `greedy_cached_channel_count()` and `greedy_prometheus_text()` for operator scrape. The Go binding carries the runtime-stub fallback (`NET_ERR_FEATURE_NOT_BUILT`) so a cdylib built without the `dataforts` feature still links cleanly into cgo programs (D-20).

---

## Data gravity (Phase 4)

Per-chain read-rate counters with exponential decay. Threshold-crossing emissions stamp `heat:<hex>=<rate>` onto the chain's existing capability announcement; greedy admission weights cache pulls by `heat × scope-match × proximity-rank`. Cold chains evict first under cluster-cap pressure; hot chains migrate toward the readers that drive the heat. No separate migration engine — gravity emerges from greedy + heat counters + capability-preference automatically.

Wires via `Redex::enable_gravity_for_greedy(mesh, DataGravityPolicy)` against an already-running greedy runtime.

### `DataGravityPolicy`

```rust
pub struct DataGravityPolicy {
    pub enabled: bool,
    pub emit_threshold_ratio: f64,         // 1.5 = re-emit when rate is 1.5× last-announced
    pub decay_half_life_secs: u64,         // 300 = 5-minute half-life
    pub tick_interval_ms: u64,             // 5000 = 5-second tick cadence
    pub normalization_reference_rate: f64, // 1000 events/s → 1.0 on the wire
}
```

### Heat counter + emission decision

`HeatCounter::observe(now, weight)` bumps the counter; `HeatCounter::current_rate(now)` returns the decayed rate. `should_emit_heat(prev, current, ratio)` is the pure-logic emission decision: emit when no prior emission, or when the current rate exceeds `prev × ratio` or falls below `prev / ratio`. Edge cases:

- **Near-zero `prev` no longer trips `inf`.** Pre-fix, a `prev` of `1e-300` with finite `current` made `current / prev` evaluate to `+inf`, which trivially satisfied any ratio check. v0.15 treats `prev` below `f64::EPSILON` (and subnormals via `is_normal()`) as "no prior emission" — the bootstrap arm runs cleanly (D-29, N-9).
- **`NaN` rates short-circuit to no-emit.** A NaN slipping into the counter (e.g. via a corrupted `to_le_bytes` round-trip on the wire) used to propagate through the ratio arithmetic. The pure function now returns `EmissionDecision::Skip` on any non-finite input.

### Wire normalization — log-scale

Pre-fix, the wire `heat:<hex>=<rate>` tag normalized `(rate / (rate + 1)).min(1.0)`, which compressed asymptotically — `rate=10` → 0.91, `rate=100` → 0.99. With `{:.2}` wire encoding, every "warm" chain looked like "blazing." v0.15 uses `ln_1p(rate) / ln_1p(reference)` with a configurable `normalization_reference_rate`; the reference defaults to 1000 events/s mapping to 1.0 on the wire. Wire format is unchanged — just the value placed on it (D-30, D-46).

### `HeatRegistry` cap + LRU

`HeatRegistry` previously grew unbounded — one entry per `(channel, origin)` pair the local node ever observed. A misbehaving peer flooding diverse origin hashes could exhaust memory before any greedy-eviction signal fired. v0.15 caps the registry at `DEFAULT_HEAT_REGISTRY_CAP = 8 * 1024` entries with LRU eviction by `last_update`; the tick loop also prunes entries with `rate == 0.0 && last_emitted == Some(0.0)` so cold chains drain on their own (D-10, N-2).

### Inbound `heat:` tag auth

Capability announcements carrying `heat:<hex>` tags previously had no provenance check at the receive side — any peer could emit a heat tag claiming any chain. v0.15 gates inbound heat tags on the publisher's existing `causal:<hex>` claim: a node advertising `heat:X` without simultaneously advertising `causal:X` has its heat tag dropped at the receive boundary (D-11). Per-peer rate-limiting of `heat:` emissions is acknowledged in D-11 + N-8 as deferred — operators see today's posture in [`CODE_REVIEW_2026_05_11_DATAFORTS.md`](../misc/CODE_REVIEW_2026_05_11_DATAFORTS.md).

### `origin_hash == 0` no longer collapses heat

Default-constructed publishers carried `origin_hash = 0`, which collapsed all unattributed chains into a single registry bucket and stamped a meaningless `heat:0000…0000` tag onto the wire. v0.15 stamps `origin_hash` from identity in `publish_to_peer` (the natural fix) and skips heat bumps when `origin_hash == 0` is observed at the gravity-runtime entry as defense-in-depth (D-9).

### `announce_heat_batch` — coalesced rebroadcast

`gravity_tick` previously walked all heat emissions and called `announce_heat` per chain. Each `announce_heat` rewrote the full `CapabilitySet::tags` vector and called `announce_capabilities` — at 100 K chains, O(n² × n_tags) per tick with each emit duplicating all chains' tags on the wire. v0.15 ships `HeatSink::announce_heat_batch`; the tick gathers all emissions, retains all stale heat tags + pushes all new heat tags in one pass, and emits a single `announce_capabilities` (D-25).

---

## BlobRef + BlobAdapter (Phase 3)

Content-addressed reference whose bytes live in the caller's existing storage (S3, Ceph, IPFS, local FS, …). The substrate carries the reference, never owns the bytes. Adapters implement `fetch` / `store` (or the streaming variants for multi-GB payloads); the FileSystemAdapter ships in-tree as the reference adapter.

### Wire format

```text
[0xB0, 0xB1, 0xB2, 0xB3]  // 4-byte magic (was single-byte 0xB0 pre-fix)
version: u8               // currently 1
hash:    [u8; 32]         // BLAKE3
size:    u64              // bytes; bounded by BlobRef::MAX_SIZE = 16 GiB
uri:     [u8]             // length-prefixed; the adapter URI scheme prefix
```

Pre-fix the discriminator was a single byte `0xB0` (D-14). A plain binary payload starting with `0xB0` would misclassify as a blob ref and route through `BlobAdapter::fetch` instead of being delivered directly. The 4-byte magic gives a collision probability of ~1 in 4 billion against arbitrary binary payloads. Old (pre-v0.15) blob refs are rejected on decode; v0.15 nodes can't exchange blob refs with pre-v0.15 nodes (Dataforts is new in v0.15, so this only matters for pre-release pilots).

`BlobRef::MAX_SIZE = 16 GiB` defaults bound the `size` field; `BlobRef::decode` and `publish_blob` reject anything larger. The previous `u64::MAX` accept-anything path could OOM on `vec![0u8; len as usize]` on 64-bit and silently truncate on 32-bit (D-15). `RedexFileConfig::blob_max_size` lifts the cap when an operator needs it.

### Adapter dispatch — URI-scheme keyed

`BlobAdapter::accepted_schemes() -> &[&str]` declares the URI schemes the adapter handles (`["s3", "s3+https"]`, `["file"]`, etc.); the registry dispatches by URI scheme, not by the channel config's `blob_adapter_id`. Pre-fix, an attacker who could write to a channel could choose its `blob_adapter_id` and route a `BlobRef` URI through any registered adapter — authority confusion (D-13). The scheme-keyed dispatch closes the gap; the channel-config-selected path is gone.

### Hash verification on store

`FileSystemAdapter::store(blob_ref, &bytes)` now BLAKE3-hashes the supplied bytes and rejects on mismatch with `blob_ref.hash`. Pre-fix the adapter wrote whatever bytes the caller passed; a content-address-violating store would silently corrupt the addressable layer (D-12). The rename-fallback path (idempotent re-store on existing content) also hash-verifies the on-disk bytes — the v0.13/v0.14-era TOCTOU on idempotent re-store via the windowed rename is closed (D-32, N-6).

`fsync` of the temp file before rename + `fsync` of the parent dir after rename land in the FileSystemAdapter store path; power loss between `rename` and OS flush previously left zero-length files in the addressable space (D-33).

### Streaming hooks

`fetch_stream(&self, blob: &BlobRef) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>` and `store_stream(&self, blob: &BlobRef, src: Pin<Box<dyn Stream<...>>>)` ship as required methods on `BlobAdapter` with default implementations that route through the existing `fetch` / `store` (so existing impls keep working); adapters wanting real streaming override the defaults. The FileSystemAdapter chunks at 256 KiB (D-16).

### Per-channel adapter override (multi-tenant)

`BlobAdapterRegistry` previously lived as a single process-wide singleton. v0.15 adds `RedexFileConfig::blob_adapter_registry: Option<Arc<BlobAdapterRegistry>>` for per-channel override; the default-tenant path uses the global singleton unchanged (D-34).

### Bounded concurrency on the FS adapter

`spawn_blocking` calls on the FileSystemAdapter are bounded via `tokio::sync::Semaphore`. Pre-fix, a fanout of concurrent stores could exhaust the tokio blocking pool and deadlock unrelated tasks (D-35).

### Conformance suite

The blob adapter conformance suite extends to cover idempotency (re-store same hash), hash-mismatch rejection, range-past-end behavior, cross-blob isolation (writes to blob A can't leak into blob B's namespace), and random-ghost reads (resolve a never-published BlobRef). Adapter authors pin against the same suite the in-tree FileSystemAdapter does (D-36).

### Cross-binding adapter authoring

Adapters can be written in the host language across every binding:

- **Python** — `PyBlobAdapter` with sync + `async def` method support. Async adapters run on a binding-owned event loop on a dedicated thread (one loop per process); calling thread sharing is via `asyncio.run_coroutine_threadsafe`. An `aiobotocore` / `httpx.AsyncClient` / SQLAlchemy async engine inside the adapter is safe — the binding never spins up a fresh `asyncio.run` per call (D-4).
- **Node** — `NodeBlobAdapter` (sync TSFN bridge) + `NodeAsyncBlobAdapter` (Promise-returning TSFN bridge).
- **C / cgo** — `NetBlobAdapterVtable` with per-field null-check at registration; partial vtables return `NET_ERR_BLOB_VTABLE_INVALID` rather than crashing on first dispatch (D-22).

`BlobError::NotFound(uri)` sanitizes the URI before including it in the error string — control chars escape as `\xNN`, length caps at 256 bytes — so a binding logging the error can't be log-injected by an attacker who controls the URI (D-31).

---

## Mesh-native blob storage (Phase 3.5)

Phase 3's `BlobRef` + `BlobAdapter` hook treats the substrate as a *carrier* for content-addressed pointers — the bytes live in S3 / Ceph / IPFS / the local FS. Phase 3.5 extends that hook with a **substrate-owned content-addressed store**: `MeshBlobAdapter` implements `BlobAdapter` against the local mesh's RedEX replication layer, registered under the `mesh://` URI scheme. A Dataforts-enabled cluster has a working blob store the moment `Redex::enable_replication(mesh)` is called; operators pick a `replication_factor` instead of standing up a separate storage system.

The full plan + design rationale lives in [`DATAFORTS_BLOB_STORAGE_PLAN.md`](../plans/DATAFORTS_BLOB_STORAGE_PLAN.md). Shipped as PR-5a through PR-5r + a post-feature hardening bundle.

### `MeshBlobAdapter`

```rust
let adapter = MeshBlobAdapter::new("mesh-prod", redex.clone())
    .with_persistent(true)
    .with_replication(ReplicationConfig::factor(3))
    .with_retention_floor(Duration::from_secs(24 * 3600))
    .with_disk_capacity(1 << 40)
    .with_auth_guard(auth_guard.clone())
    .with_blob_heat(blob_heat_registry, Duration::from_secs(60));
```

Implements `BlobAdapter::{store, fetch, fetch_range, exists, delete, stat, prefetch}` plus `store_stream` / `fetch_stream` for multi-GB payloads. `store` BLAKE3-verifies the supplied bytes against `blob_ref.hash` before persisting; idempotent — repeated stores of identical bytes against the same hash are a no-op. Chunks above the 4 MiB threshold split into independently-content-addressed `RedexFile`s, with a small manifest blob (one `BlobRef::Manifest`) carrying the chunk list.

### `BlobRef::Manifest` + chunking

`BlobRef` gains a `Manifest { encoding, chunks: Vec<ChunkRef>, size }` variant alongside the v0.15 `Small`. Wire form is forward-compatible — the 4-byte magic + version byte gate variants. `Encoding::Replicated` ships in v0.2; `Encoding::ReedSolomon { k, m }` is reserved on the wire for v0.3. Chunking is fixed-size 4 MiB; a 16 GiB blob holds 4096 chunk references (≈144 KiB manifest, within the inline path itself).

### `publish_with_blob` — store-then-publish

```rust
let receipt = mesh.publish_with_blob(
    channel,
    payload_bytes,
    BlobDurability::ReplicatedTo(3),
).await?;
```

Stores the bytes to the configured durability, then publishes an event referencing the resulting `BlobRef`. The receipt carries a `WriteToken` whose `applied_through_seq` watermark composes with Phase 5's read-your-writes — a consumer calling `tasks.wait_for_token(token, deadline)` blocks until both the publish event has folded and the chunks have replicated to the requested durability. `BlobDurability::{BestEffort, DurableOnLocal, ReplicatedTo(n)}` chooses the trade-off between latency and the durability guarantee the receipt asserts.

### Refcount + GC + pinning

`BlobRefcountTable` tracks per-hash references from three sources: RedEX chain folds (PR-5h wires greedy into the increment / decrement path on cache admit / eviction), CortEX adapters indexing events, and direct `pin(blob_ref)` / `unpin(blob_ref)` operator calls. `sweep_gc(now, disk_pressure)` collects refcount = 0 + unpinned hashes whose `first_seen` is older than the retention floor (default 24 h); `disk_pressure = true` bypasses the floor for emergency reclaim. `delete_chunk` drops the refcount entry inline rather than waiting for the sweep.

A health gate advertises `dataforts:blob-storage-unhealthy` when local disk crosses 95 % and clears at 85 % (hysteresis); other nodes' admission filters reject inbound migrations to an unhealthy node.

### Capability extension

Three new capability families compose against the existing 5-axis `PlacementFilter`:

- **`BlobCapability`** — `storage`, `disk_total_gb`, `disk_free_gb`, `class`.
- **`GreedyCapability`** — `enabled`, `scope`, `proximity`. Same shape as the chain-side greedy gate; blobs reuse the chain proximity score.
- **`GravityCapability`** — `enabled`, `scope`, `proximity`. Independent of greedy; a node can participate in gravity migration without speculatively greedy-pulling.

`PlacementFilter` gains an `Artifact::Blob { blob_hash, size_bytes, encoding, capabilities }` variant; the score function reads `blob.disk_free_gb` + `blob.storage` + `gravity.scope` to gate blob placement.

`TopologyScope` (Node ⊂ Zone ⊂ Region ⊂ Mesh) is a hard boundary on greedy / gravity decisions — `scope == Zone` means the local node never pulls or accepts migration of a blob whose publisher is in a different zone.

### G-1 / G-2 / G-3 — admission, gravity, migration

Three pure-logic decision primitives plus the runtime that consumes them:

- **`should_pull_blob(local_caps, publisher_caps)` (G-1).** Greedy admission verdict: `Admit` / `Reject(reason)` where `reason ∈ { NoStorageCap, GreedyDisabled, ProximityZero, Unhealthy, ScopeMismatch }`. Wired into `GreedyRuntime::dispatch_event` so admitted chains carrying `BlobRef`s trigger a `BlobAdapter::prefetch` on the referenced blob. Counters: `dataforts_greedy_blob_pulls_admitted_total` / `…_rejected_total{reason}`.
- **`should_migrate_blob_to(target_caps, publisher_caps, size_bytes)` (G-2 / G-3).** Gravity migration verdict for `target_caps`; extends the `should_pull_blob` shape with a `disk_free_gb` headroom check (rounded up — `1.5 GiB blob → ceil(1.5) = 2 GiB required`). `MigrateBlobReject::InsufficientDisk` is the additional variant.
- **`drive_blob_migration_tick(local_caps, capability_index, adapter, size_resolver)`** + the `_with_manifest_resolver` variant. Walks peers in the capability index, parses `heat:blob:<hex>=<rate>` reserved tags via `parse_blob_heat_tag`, runs `should_migrate_blob_to` against each candidate, and on admit calls `adapter.prefetch`. The manifest-resolver variant recursively prefetches every constituent chunk of a `BlobRef::Manifest` (PR-5o). Returns a `BlobMigrationTickReport` with per-reason counters for operator dashboards.

Per-node pull, not centralized push — each node decides what to pull from its local capability view. The plan documents the storage-overflow push-to-peer track as deferred future work.

### Blob heat — `heat:blob:<hex>=<rate>` tags

Mirrors the chain-side gravity layer with a key-shape change: blob heat keys on the 32-byte chunk hash. `BlobHeatRegistry` (LRU + cap + half-life decay, same discipline as `HeatRegistry`); `MeshBlobAdapter::with_blob_heat(registry, half_life)` opts the adapter into bumping heat on every successful `fetch` / `fetch_range`. `MeshBlobAdapter::tick_blob_heat(policy, sink)` walks the registry and routes `Emit { rate }` / `Withdraw` decisions through the `BlobHeatSink` trait; `MeshNode` implements the sink by adding a `heat:blob:<hex64>=<rate>` reserved tag to the local capability set and rebroadcasting via `announce_capabilities`.

The `blob:` body sub-prefix keeps blob-heat tags disjoint from chain-heat tags on the wire (`heat:<origin_hex>=<rate>` for chains, `heat:blob:<hash_hex>=<rate>` for blobs).

### G-6 — Auth

`pin_authorized` / `unpin_authorized` / `delete_chunk_authorized` gate on `AuthGuard::is_authorized_full(origin, channel)` against the chain that originally published the blob. The unauth `pin` / `unpin` / `delete_chunk` variants remain available for system-internal callers (GC sweep, chain-fold refcount increment / decrement). `BlobError::Unauthorized` is the typed rejection.

### `net-blob` operator CLI

Operator surface shipped behind the new `cli` Cargo feature (`features = ["dataforts", "redex-disk", "cli"]`). Subcommands:

- `net-blob put <path>` — store + return the resulting `BlobRef`.
- `net-blob get <hash> --out <path>` — fetch; refuses to clobber existing output files.
- `net-blob exists <hash>` — exit 0 if present, exit 1 if absent.
- `net-blob stat <hash>` — refcount + size + last-seen.
- `net-blob ls` — list known content hashes.
- `net-blob pin <hash>` / `net-blob unpin <hash>` — operator pin / unpin.
- `net-blob gc [--retention <duration>] [--dry-run] [--disk-pressure]` — GC sweep. `--dry-run` lists candidates; `--disk-pressure` bypasses the retention floor.
- `net-blob metrics` — Prometheus text body.

`--format json` is available across every subcommand for scripting; `parse_duration` accepts `30s` / `5m` / `1h` / `24h` / `7d`.

### Cross-binding — Python

`net.MeshBlobAdapter` lands in the Python binding behind `--features dataforts`. Methods: `store(blob_ref, data)`, `fetch(blob_ref) -> bytes`, `fetch_range(blob_ref, start, end) -> bytes` (half-open `[start, end)`), `exists(blob_ref) -> bool`, `prometheus_text() -> str`. Plus a `PyBlobRef` constructor taking `(uri, hash_bytes, size)` and round-tripping through `encode()` / `BlobRef.from_encoded(bytes)`. Persistent mode (`MeshBlobAdapter(redex, "id", persistent=True)`) writes per-chunk `RedexFile`s to disk.

Node + Go binding wrappers for the v0.2 `MeshBlobAdapter` surface are tracked as deferred per-binding follow-ups in the plan doc.

### Hardening — post-PR-5j review pass

Eighteen commits between PR-5r and the v0.15 cut closed second-pass review items. Grouped by area:

**DoS surfaces**
- `MeshNode::filter_unauthorized_heat_tags` caps incoming `heat:blob:` tags at 256 per announcement; the cap bounds migration-controller amplification (each surviving heat tag drives a `prefetch` attempt).
- `CapabilityIndex::by_origin_hash` is a `u32`-truncated shortcut; an `AtomicU64 collision_count` field surfaces last-writer-wins collisions on the admission hot path for operator observability (a wire-format-preserving fix; full collision-safe indexing is out of scope for v0.15).
- `BlobMigrationController` caps per-peer prefetch admits per tick so a single peer can't dominate the disk-bandwidth budget.
- Per-channel `chain_blob_refs` shadow set in the greedy runtime is bounded; a misbehaving publisher can't inflate per-channel memory unboundedly.

**Soundness**
- Python `&[u8]` adapter parameters (`PyMeshBlobAdapter::store`, `blob_publish`, `blob_resolve`) now copy bytes under the GIL (`data.to_vec()`) before `py.detach()`. PyO3 0.28's strict `&[u8]` type-rejects `bytearray` at the FFI boundary; the post-fix copy keeps the capture-then-detach pattern safe against a hypothetical future PyO3 relaxation.
- `CapabilityIndex` fails closed when a wire `u32 origin_hash` is ambiguous and falls back to the empty-caps default for vacant slots.
- `MeshBlobAdapter` serializes concurrent stores against the same hash through a per-hash lock and BLAKE3-verifies bytes already on disk match the content address before short-circuiting the idempotent re-store path.

**Races**
- `gravity_tick` captures sink + emissions + policy under one read of the gravity RwLock. Pre-fix it took the lock twice; a concurrent `set_gravity` / `clear_gravity` between reads could renormalize emissions computed under policy A against policy B.
- `drive_blob_migration_tick_with_manifest_resolver` only inserts hashes into the dedup set after a successful Admit + Ok prefetch; rejected siblings + prefetch errors stay reconsiderable when the same hash surfaces under a later candidate's manifest expansion.
- `BlobMigrationController` floors the publisher-scope check at the narrowest claim across all heat advertisers for the same hash so a single broad-scope peer can't bypass a narrower-scope peer's gate.

**Label injection**
- Operator-supplied `adapter_id` is escaped per the Prometheus text-exposition spec (`\\`, `\"`, `\n`) before being interpolated into label values. A `--adapter-id 'evil"\n# bogus_metric{} 1\n#'` payload can't inject fake metric lines.

**Operator-surface hardening**
- `net-blob get --out` refuses to clobber existing output files (the CLI may run with elevated privileges).
- `delete_chunk` drops the refcount entry inline rather than waiting for `sweep_gc`.
- `BlobError::Unauthorized` typed variant separates auth-rejection from other rejection modes.

**Build graph**
- `dataforts = ["redex", "redex-disk", "dep:blake3"]`. `--features dataforts` alone previously failed to compile because the blob path calls `RedexFile::sync()` which is gated behind `redex-disk`. The feature graph now encodes the actual dep.

**Doc + test-name polish**
- Two `pull_rejects_*` admission tests asserted `Admit` (Zone-narrower-than-Mesh + absent-publisher-scope-defaults-to-Mesh) — renamed to `pull_admits_*`.
- `controller_skips_peers_without_blob_heat_tags` renamed to `controller_ignores_chain_heat_shape_tags`.
- `BlobRef::encoded_len` doc now documents Small as O(1) and Manifest as full-encode-cost (was "cheap for both variants").
- `PyMeshBlobAdapter::fetch_range` doc spells out half-open `[start, end)` tied to Python slice semantics.
- `publish_with_blob` doc drops the overstated atomicity claim and documents chunk-advertise ordering inline.

The full per-commit log lives in the plan doc's [Shipping status](../plans/DATAFORTS_BLOB_STORAGE_PLAN.md#shipping-status) table under "Hardening — post-PR-5j hardening pass."

---

## Active blob overflow (Phase 3.5 / v0.3 blob track)

v0.2 mesh-native blob storage is intentionally pull-only — when a node fills up, it advertises `dataforts:blob-storage-unhealthy` and other nodes' admission rejects inbound migrations. The local node never *pushes* its own blobs elsewhere; under sustained saturation a node either reclaims via GC or stops accepting new bytes. The **v0.3 active-overflow extension** closes the loop: when a node fills up, it picks coldest blobs by inverse blob-heat and pushes them to peers that have free disk and have opted into receiving overflow.

The plan + design rationale lives in [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`](../plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md). Shipped as P1..P5 across five commits on the `dataforts-overflow` branch.

### Disabled by default, one boolean to turn on

Active overflow is **off** in v0.2 deployments — every existing call site keeps the v0.2 pull-only posture without code changes. To opt in, operators flip a single boolean on the adapter:

```rust
// Construction-time, simple form:
let adapter = MeshBlobAdapter::new("mesh-prod", redex.clone())
    .with_overflow(OverflowConfig { enabled: true, ..Default::default() });

// Or with typed tunables:
let adapter = MeshBlobAdapter::new("mesh-prod", redex.clone())
    .with_overflow(OverflowConfig {
        enabled: true,
        high_water_ratio: 0.80,
        low_water_ratio: 0.65,
        max_pushes_per_tick: 8,
        scope: TopologyScope::Zone,
        tick_interval_ms: 30_000,
    });

// Runtime toggle — no rebuild:
adapter.set_overflow_enabled(true);
adapter.set_overflow_enabled(false);
```

When enabled, the adapter advertises `dataforts.blob.overflow` on its capability set; peer-selection on the push side filters by this tag so overflow targets only nodes that have themselves opted in. Symmetric opt-in: the receive-side admission gate rejects pushes from a sender that *isn't* overflow-enabled.

### `OverflowConfig` thresholds

```rust
pub struct OverflowConfig {
    pub enabled: bool,                  // master switch
    pub high_water_ratio: f64,          // 0.85 default — triggers tick
    pub low_water_ratio: f64,           // 0.70 default — clears tick (hysteresis)
    pub max_pushes_per_tick: usize,     // 16 default — bandwidth burst cap
    pub scope: TopologyScope,           // Mesh default — push-target scope bound
    pub tick_interval_ms: u64,          // 30_000 default
}
```

Hysteresis mirrors the existing `dataforts:blob-storage-unhealthy` health-gate (95% / 85%) with looser thresholds because overflow fires *before* the unhealthy advertisement — by the time a node is unhealthy, overflow has already been shedding for a while.

### G-7 — Active overflow admission

```rust
pub fn should_accept_overflow_from(
    local_caps: &CapabilitySet,
    sender_caps: &CapabilitySet,
    blob_size_bytes: u64,
) -> OverflowVerdict;
```

Receive-side mirror of `should_migrate_blob_to`. Six ordered gates: `NoStorageCap` → `NotParticipating` → `SenderNotOverflowing` → `Unhealthy` → `ScopeMismatch` → `InsufficientDisk`. Each `OverflowReject` variant maps to a distinct Prometheus counter label so operators dashboard both sides.

The ordering matters operationally: a compute-only node surfaces `NoStorageCap` rather than `NotParticipating`, even when both gates would reject — the most actionable signal wins.

### `BlobOverflowController` + tick driver

```rust
pub struct BlobOverflowController<'a> {
    pub local_caps: &'a CapabilitySet,
    pub capability_index: &'a CapabilityIndex,
    pub heat_registry: &'a Arc<Mutex<BlobHeatRegistry>>,
    pub refcount: &'a BlobRefcountTable,
    pub config: &'a OverflowConfig,
}
```

The controller's `candidates(now, size_for_hash)` walks the heat registry in ascending-rate order (coldest first), filters out pinned + non-zero-refcount hashes, and for each remaining candidate selects an overflow-enabled peer with sufficient disk-free + matching scope. Target ranking: highest `disk_free_gb` wins (greedy spread across peers); ties broken by lowest `node_id` for determinism.

`drive_blob_overflow_tick` composes the controller + hysteresis state machine + the `OverflowPushSink` trait:

```rust
pub async fn drive_blob_overflow_tick(
    controller: &BlobOverflowController<'_>,
    sink: &dyn OverflowPushSink,
    observation: OverflowTickObservation<'_>,
    size_for_hash: impl Fn([u8; 32]) -> Option<u64>,
) -> BlobOverflowTickReport;
```

`OverflowTickObservation` bundles per-tick state (disk stats, hysteresis atomic, clock). The `BlobOverflowTickReport` carries every counter the Prometheus emitter needs.

`MeshBlobAdapter::drive_overflow_tick(ctx, size_for_hash)` is the 2-arg convenience wrapper — composes the controller, threads the adapter's `refcount` / `config` / `overflow_active`, runs the tick, auto-records the report into the adapter's metrics.

### Wire protocol — `OverflowPush` RPC

```rust
pub struct OverflowPush {
    pub blob_hash: [u8; 32],
    pub size_bytes: u64,
    pub sender_node_id: u64,
}

pub enum OverflowPushAck {
    Accepted,
    Rejected(OverflowReject),
    OpenChunkFailed,
}
```

The chunk bytes themselves don't ride this RPC — the nudge tells the receiver to open the chunk channel against its local Redex with replication armed; the existing per-chunk replication runtime pulls the bytes from any holder advertising `causal:<hash>` (typically the sender). The RPC routes through the existing nRPC machinery under the `dataforts.blob.overflow_push` service name.

- **Sender side**: `MeshNode::send_overflow_push(target, hash, size) -> Result<OverflowPushAck, BlobError>` — encodes the request, dispatches via `MeshNode::call`, decodes the typed ack.
- **Receiver side**: `MeshNode::serve_overflow_push(adapter) -> ServeHandle` registers the `OverflowPushHandler` under the service name. Each inbound request reads live `user_caps_snapshot` + the capability index, runs admission, on Admit calls `adapter.prefetch(BlobRef::small(...))` to open the chunk channel.
- **`MeshNodeOverflowPushSink`** — concrete `OverflowPushSink` impl wrapping `Arc<MeshNode>`. Maps non-Accepted acks to typed `BlobError::Backend` so the controller's `push_errors` counter bumps uniformly.

`OverflowReject` carries `serde::{Serialize, Deserialize}` so the typed reason rides inside `OverflowPushAck::Rejected` across the wire intact.

### Prometheus counter family

The adapter's `prometheus_text()` body emits the full overflow surface:

```text
dataforts_blob_overflow_pushes_admitted_total{adapter="..."}     <counter>
dataforts_blob_overflow_push_errors_total{adapter="..."}         <counter>
dataforts_blob_overflow_pushed_bytes_total{adapter="..."}        <counter>
dataforts_blob_overflow_rejected_no_target_total{adapter="..."}  <counter>
dataforts_blob_overflow_rejected_total{adapter="...",reason="no_storage_cap"}        <counter>
dataforts_blob_overflow_rejected_total{adapter="...",reason="not_participating"}     <counter>
dataforts_blob_overflow_rejected_total{adapter="...",reason="sender_not_overflowing"} <counter>
dataforts_blob_overflow_rejected_total{adapter="...",reason="unhealthy"}             <counter>
dataforts_blob_overflow_rejected_total{adapter="...",reason="scope_mismatch"}        <counter>
dataforts_blob_overflow_rejected_total{adapter="...",reason="insufficient_disk"}     <counter>
dataforts_blob_overflow_high_water_triggered_total{adapter="..."} <counter>   # false→true edges
dataforts_blob_overflow_low_water_cleared_total{adapter="..."}    <counter>   # true→false edges
dataforts_blob_overflow_active{adapter="..."}                     <gauge 0/1>
dataforts_blob_overflow_disk_ratio{adapter="..."}                 <gauge 0..1>
```

Sender's `push_errors_total` bumps on every non-Accepted ack (RPC transport + admission rejection + chunk open failure). The receiver's `rejected_total{reason}` family bumps on each admission rejection by variant — operators dashboarding both sides see matching volumes.

Hysteresis transitions only bump on the **edge**: `false → true` increments `high_water_triggered_total`, `true → false` increments `low_water_cleared_total`. Repeated active-during ticks don't bump either counter, so the metrics count distinct "overflow episodes" rather than steady-state ticks.

### `net-blob overflow status` CLI

```text
net-blob overflow status
net-blob --format json overflow status
```

Prints the configured boolean, the runtime `overflow_active` flag (set by the most recent tick on this process), the configured thresholds, and the cumulative counter family. JSON form is shape-stable: top-level keys `adapter` / `config` / `active` / `counters`, with every per-reason counter present even at zero (operator dashboards don't want missing keys).

### Cross-binding — Python

`MeshBlobAdapter` gains an `overflow` kwarg + getter / setter surface:

```python
from net import MeshBlobAdapter, Redex

redex = Redex(persistent_dir="/data/blobs")

# Simple boolean — turn on with defaults.
adapter = MeshBlobAdapter(redex, "py-prod", overflow=True)

# Typed dict — turn on + tune thresholds. Missing keys inherit defaults.
adapter = MeshBlobAdapter(
    redex,
    "py-prod",
    overflow={"high_water_ratio": 0.90, "max_pushes_per_tick": 4, "scope": "zone"},
)

# Pre-stage config without flipping the switch (e.g. for testing).
adapter = MeshBlobAdapter(
    redex,
    "py-stage",
    overflow={"enabled": False, "high_water_ratio": 0.95},
)

# Runtime control.
adapter.set_overflow_enabled(True)
adapter.set_overflow_config({"enabled": True, "high_water_ratio": 0.88, ...})

# Read-only inspection.
adapter.overflow_enabled   # bool — master switch state
adapter.overflow_active    # bool — runtime hysteresis state
adapter.overflow_config    # dict — full typed snapshot
```

The dict path enforces typed-error contracts: unknown keys raise `TypeError` (typo defense — `high_water_ration` doesn't silently fail); invalid scope strings raise `ValueError`. Node + Go bindings follow per-binding cadence (consistent with the v0.2 deferred-binding posture).

### Storage layout + safe-delete

Sender doesn't immediately delete the local copy on `OverflowPushAck::Accepted` — the durability watermark observation (sender polls capability index for receiver's `causal:<hash>` advertisement) is deferred to a future P6 follow-up. Today the local copy stays until the standard GC sweep collects it under retention + refcount-zero.

This is conservative-by-default: the receiver may have admitted but the chunk-pull could still fail before the bytes land. Operators running into "sender disk doesn't drain fast enough" today can flip `gc --disk-pressure` (which bypasses the retention floor for refcount-zero hashes) — the explicit watermark gate lands in v0.16+.

### Hardening — clippy + arg-bundling

The `OverflowTickContext<'a>` + `OverflowTickObservation<'a>` borrow structs bundle the tick-driver args so neither `drive_blob_overflow_tick` (4 args) nor `MeshBlobAdapter::drive_overflow_tick` (2 args) trips clippy's `too_many_arguments` lint. No `#[allow(clippy::too_many_arguments)]` anywhere in the overflow surface — the bundling earns the clean signatures.

### Test coverage

- **P1**: 17 pure-logic tests (`should_accept_overflow_from` × 8 reject variants + admit path + ordering, `BlobCapability::overflow_enabled` round-trip × 2, `OverflowConfig` adapter surface × 5).
- **P2**: 20 controller / tick / hysteresis tests (`step_overflow_hysteresis` × 4 edge cases, `BlobOverflowController::candidates` × 7 filter paths, tick-driver tests × 6 against an `OverflowPushRecorder` mock, `scope_covers` × 2, `MeshBlobAdapter::overflow_active` shared-state × 1).
- **P3**: 7 wire-format + integration tests (postcard round-trip × 5 variants + 2-node `MeshNode::send/serve_overflow_push` end-to-end × 2).
- **P4**: 10 metrics + CLI tests (`record_overflow_tick` bumps × 4 paths, per-reason `record_overflow_reject` × 1, Prometheus body shape × 2, CLI `overflow status` Human + JSON + metrics-body inclusion × 3).
- **P5**: 12 Python pytest tests (default-off + bool-true + bool-false + dict-overrides + dict-prestage + scope-parsing + unknown-key + bad-scope + bad-type + runtime-setter + whole-config-setter + round-trip × 12).

Total: 66 new tests across the v0.3 overflow track.

---

## Read-your-writes (Phase 5)

Every successful `Tasks::create` / `Memories::insert` / etc. returns a `WriteToken { origin_hash, seq }`. Pass it to `wait_for_token(token, deadline)` and the call blocks until the local fold has actually applied that sequence number — not just folded it. A producer reads its own write through the cache deterministically; no busy-poll, no time-window heuristic.

### `WriteToken`

```rust
pub struct WriteToken {
    pub(crate) version: u8,
    pub(crate) origin_hash: u64,
    pub(crate) seq: u64,
}
```

Fields are `pub(crate)`; the public constructor is `#[doc(hidden)]`. `FromStr` is gated behind `#[cfg(test)]` or the `wire-debug` feature. Tokens are unforgeable only against the adapter that issued them (via origin binding); the threat model is documented inline (D-19).

### `wait_for_token` — applied vs. folded

Pre-fix, `wait_for_token` delegated to `wait_for_seq`, which returned when the folded watermark passed `seq` — including events that `FoldErrorPolicy` silently skipped via `RedexError::is_recoverable_decode`. A producer whose write hit a skip got `Ok(())` and then read state that didn't reflect its write.

v0.15 adds `applied_through_seq()` (events that actually ran through the fold) alongside the existing `folded_through_seq()` (events the fold saw). `wait_for_token` waits on **applied**, not folded; skipped events are no longer auto-acknowledged (D-17).

### `FoldStopped` error variant

`wait_for_seq` previously returned `Ok` when `running == false` (the fold task crashed under `FoldErrorPolicy::Stop`). Every pending RYW wait resolved with a silent `Ok(())` even though `seq` was never folded. v0.15 adds `WaitForTokenError::FoldStopped { applied_through_seq }`; the wait path checks `applied_through_seq >= seq` when it wakes due to `running == false` and surfaces the typed error when the fold actually stalled (D-18).

### Non-blocking poll — `deadline_ms == 0`

`wait_for_token(token, 0)` now does a synchronous applied-vs-token check and returns `Ok(())` / `Err(Timeout)` / `Err(FoldStopped)` without scheduling a wait. Pre-fix the FFI rewrote `0` to `1 ms`, costing a real wait round-trip for a "is fold caught up?" probe (D-23). The synchronous-poll behavior is consistent across the FFI / Node / Go / Python surfaces; the Python surface promoted `poll_for_token` to the public API alongside `wait_for_token` so non-async Python callers can probe without spawning a task (N-4).

### Process-wide in-flight cap

The 1024-deep wait-queue cap was per-adapter pre-fix; a process with 100 channels could stack 100 K outstanding RYW waiters. v0.15 ships `set_global_ryw_inflight_cap(usize)` for a process-wide bound; every `wait_for_token` call does a two-tier acquire (process-wide first, then per-adapter). The semaphore is renamed `ryw_inflight_cap` with a non-FIFO documentation note (the current implementation is `Semaphore::try_acquire`; true FIFO is deferred) (D-37, D-38).

### Cross-binding API surface

| Binding | Surface |
|---------|---------|
| **Rust** | `tasks.wait_for_token(token, Duration)` / `memories.wait_for_token(token, Duration)`; `tasks.poll_for_token(token)` synchronous variant |
| **Python** | `tasks.wait_for_token(token, deadline_ms=…)`; `deadline_ms=0` is a non-blocking poll (N-4) |
| **Node** | `tasks.waitForToken(token, deadlineMs)`; `deadlineMs === 0` is a non-blocking poll |
| **Go** | `tasks.WaitForToken(token, timeout)` + `tasks.PollForToken(token)` + `tasks.WaitForTokenContext(ctx, token)` non-blocking variant; Go context cancellation isn't propagated into the FFI wait — see `WaitForTokenContext` rustdoc for the contract (D-45, N-11) |
| **C** | `net_tasks_wait_for_token` / `net_memories_wait_for_token`; `timeout_ms == 0` is a non-blocking poll. Every FFI entry wraps `block_on` in `std::panic::catch_unwind(AssertUnwindSafe(…))`; panics surface as `NET_ERR_PANIC` rather than unwinding across `extern "C"` (D-21) |

---

## Channel-hash widening — `u16` → `u32` canonical

The wire `NetHeader::channel_hash` (16 bits, 65 536 buckets) routinely collides at mesh scale — birthday-paradox threshold ~300 channels. Pre-fix every substrate decision keyed on the wire `u16`: ACL (AuthGuard), storage (Redex), config (ChannelConfigRegistry), token (PermissionToken), RYW. Two unrelated channels colliding on `u16` shared one ACL decision, one RedexFile, one config row.

v0.15 widens the canonical channel hash to **`u32` substrate-wide** while keeping the wire `NetHeader::channel_hash` at `u16` — the per-packet width is fixed by the 64-byte cache-line-aligned header budget. The wire `u16` is now a fast-path filter hint only; wire-side collisions are benign because every non-fast-path decision (auth / storage / config / RYW) keys on the canonical 32-bit hash via registry-side disambiguation.

Mirrors the `origin_hash u64-canonical / u32-wire` precedent set in v0.13: per-packet width fixed, application layer wider, narrowing helper at the wire boundary.

### Canonical type

```rust
pub type ChannelHash = u32;

impl ChannelName {
    pub fn hash(&self) -> ChannelHash { … }        // canonical u32
    pub fn wire_hash(&self) -> u16 { … }            // wire fast-path hint
}
pub fn channel_hash(name: &str) -> ChannelHash { … }
pub fn wire_channel_hash(name: &str) -> u16 { … }
```

`ChannelHash` joint-collision threshold is ~65 K channels per process (above realistic deployment), so the canonical key is treated as collision-free in fast paths.

### `ChannelConfigRegistry` — dual index

```rust
pub struct ChannelConfigRegistry {
    configs: DashMap<String, ChannelConfig>,
    by_hash: DashMap<ChannelHash, Vec<String>>,    // canonical (u32, rare collisions)
    by_wire_hash: DashMap<u16, Vec<String>>,       // wire (u16, routine collisions)
    prefix_configs: DashMap<String, ChannelConfig>,
}
```

`get(canonical)` returns `None` on the rare canonical collision (forces caller fallback); `get_by_wire_hash(wire)` returns `None` on wire-bucket collision (used by receive-side dispatch, contrast with `ChannelRegistry::get_all_by_wire_hash` below). Removals stay collision-safe — `remove(canonical)` keys on the unique canonical hash; `remove_by_name(name)` is the explicit-name path.

### `ChannelRegistry` — return the full collision set

`ChannelRegistry::get_by_wire_hash` was renamed to `get_all_by_wire_hash` and explicitly returns the full collision-bucket vector. This contrasts with `ChannelConfigRegistry::get_by_wire_hash`, which returns `None` on collision to force a safe default at the policy layer. The naming asymmetry is intentional — operators querying "what channels share this wire bucket" want the full set; the policy layer querying "what's the config for this packet" wants a unique answer or nothing.

### `AuthGuard` — canonical u32 ACL

The bloom-filter key buffer widens from 10 to 12 bytes (`u64 origin_hash + u32 channel_hash`); `check_fast` / `authorize` / `revoke` signatures all take `ChannelHash`. The two-tier authorization shape (fast-path bloom + verified cache + exact-name backstop) is unchanged; the canonical hash makes the fast-path bloom collision-resistant at realistic scale. The exact-name backstop remains the only collision-free path for control-plane / storage authorization decisions where adversarial canonical-hash collisions matter.

### `PermissionToken` — 161-byte wire form

```text
issuer:           32 bytes (EntityId)
subject:          32 bytes (EntityId)
scope:             4 bytes (u32)
channel_hash:      4 bytes (canonical ChannelHash, u32; was u16)
not_before:        8 bytes (u64 unix timestamp)
not_after:         8 bytes (u64 unix timestamp)
delegation_depth:  1 byte  (u8)
nonce:             8 bytes (u64)
--- signed ---
signature:        64 bytes (ed25519)
```

Total: 161 bytes (was 159). Signed payload: 97 bytes (was 95). `PermissionToken::from_bytes` rejects 159-byte input as `TokenError::InvalidFormat`; old tokens must be reissued under the wider form.

### RPC inbound dispatcher — `(canonical, dispatcher)` pairs

`MeshNode::register_rpc_inbound(channel_hash: ChannelHash, dispatcher)` takes the canonical hash. The dispatcher map is indexed by the wire `u16` for O(1) lookup on the inbound packet decode path; each bucket stores a `Vec<(ChannelHash, RpcInboundDispatcher)>` so wire-bucket collisions between independently-registered canonical channels don't share a dispatcher slot. `RpcInboundEvent::channel_hash` is the canonical `u32` — dispatchers receive the disambiguated identity.

### Five post-merge follow-up commits

A focused review pass landed five hardening commits on the `channel-hash-32` branch after the primary widening:

| # | Commit | Concern | Test added |
|---|---|---|---|
| 1 | `fda25a7d` | Race in `unregister_rpc_inbound` clobbering a concurrent sibling register | sibling-survives + race-stress |
| 2 | `af5f6c25` | Stale "16-bit verified cache" comment in `mesh.rs` | n/a (doc) |
| 3 | `1fb62fbc` | Stale 16-bit framing in two regression test docstrings | n/a (doc) |
| 4 | `c141e691` | Per-packet `Vec` allocation in the dispatch fast path | end-to-end wire-bucket collision fan-out |
| 5 | `c5e75ff6` | `get_by_wire_hash` semantics divergence between registries | full-collision-set contract test |

The single-dispatcher hot path (the overwhelming case at typical sizing) avoids the heap allocation entirely; the collision-set vector is built only when a wire bucket has more than one canonical entry. The unregister race is closed by atomic remove-if-present semantics; concurrent register + unregister can no longer leave the map in a torn state.

### Dataforts greedy stays on wire `u16`

The greedy data-plane cache deliberately keys on the wire `u16` (not the canonical `u32`) because the wire hash is what the inbound packet that triggered the observe call carries — there's no canonical lookup at packet decode time. The cache file is named `dataforts/greedy/<hex16>`; two wire-colliding channels share a cache file (a small mix-up at the data-plane layer; ACL and storage decisions stay collision-safe via the canonical hash).

---

## Hardening — `dataforts-feature` two-pass review

Two coordinated review passes landed before the v0.15 branch cut. The primary review on the `dataforts-feature` branch surfaced 54 numbered items (D-1..D-54): 4 blockers, 19 highs, 24 mediums, 7 lows. An independent second pass on 2026-05-12 surfaced 11 N-series items (N-1..N-11): 3 highs, 6 mediums, 2 lows. All but three closed before merge (deferred with rationale in the tracking doc). The closures group by area:

### Greedy correctness (D-1..D-8 + D-25..D-28 + N-5)

- Cluster-cap eviction withdraws chain announcements inline (D-1).
- Bandwidth-budget rejection bumps a distinct counter rather than dropping events silently (D-2).
- `upsert` on update subtracts the old `bytes` before replacing the file pointer (D-3).
- `chain_caps` resolves the chain publisher's caps via the capability index, not the last-hop peer's (D-5).
- TOCTOU on `is_new_channel` collapsed into a single locked get-or-insert (D-6).
- `tokio::spawn` per inbound event bounded by a semaphore (D-7).
- `colocation_target_held` resolved from the cache map, not hardcoded `None` (D-8).
- `gravity_tick` coalesces N×`announce_capabilities` into one via `announce_heat_batch` (D-25).
- Retention-trim drift on `entry.bytes` resyncs via `RedexFile::retained_bytes` (D-26).
- 5 cache-lock acquisitions per dispatch coalesced to 1 in the steady-state path; new-channel path takes 2 with TOCTOU re-check (D-28).
- Eviction explicitly drops `gravity.heat` lock before calling `sink.withdraw_chain` to avoid lock-ordering hazards (N-5).

### Gravity correctness (D-9..D-11 + D-29..D-30 + N-2 + N-9)

- `origin_hash == 0` no longer collapses per-chain heat (D-9, fix at publish-side + defense-in-depth at gravity-runtime entry).
- `HeatRegistry` bounded + LRU-evicted + tick-pruned (D-10, N-2).
- Inbound `heat:` tags gated on the publisher's matching `causal:` claim (D-11; per-peer rate-limit deferred per N-8).
- `should_emit_heat` subnormal-safe via `is_normal()` + EPSILON-floor (D-29, N-9).
- Log-scale wire normalization with configurable reference rate (D-30 / D-46).

### Blob correctness (D-12..D-16 + D-31..D-36 + D-49..D-50 + D-52..D-53 + N-3 + N-6 + N-7)

- `FileSystemAdapter::store` hash-verifies bytes (D-12).
- URI-scheme keyed adapter dispatch closes authority confusion (D-13).
- 4-byte magic for `BlobRef` discriminator closes payload-misclassification (D-14).
- `BlobRef` size bounded; `fetch_range` guards `usize` cast (D-15).
- Streaming hooks on `BlobAdapter` (D-16).
- Log injection via `BlobError::NotFound(uri)` sanitized (D-31).
- Unique-suffix temp filenames (D-32); `fsync` of temp + parent dir (D-33).
- Per-channel `BlobAdapterRegistry` override (D-34).
- Bounded concurrency on `spawn_blocking` (D-35).
- Conformance suite extended with idempotency / hash-mismatch / range-past-end / cross-blob isolation / random-ghost (D-36).
- `BlobError` marked `#[non_exhaustive]` (D-49).
- `RedexFileConfig::blob_adapter_id` unset surfaces the right error variant (D-50).
- `OpaqueCtx(AtomicPtr<c_void>)` collapsed to plain `*mut c_void` (D-52).
- Adapter timeout user-tunable (D-53).
- `path_for` defends against symlinks in the shard root via canonicalize (N-3).
- Windows rename-fallback TOCTOU on idempotent re-store hash-verifies existing content (N-6).
- `catch_unwind` + caller-held locks documented as a hazard in `ffi/mod.rs` + per-binding READMEs (N-7).

### RYW correctness (D-17..D-19 + D-37..D-38 + D-45 + D-51 + N-4 + N-11)

- `wait_for_token` waits on applied seq, not folded (D-17).
- `FoldStopped` error variant when fold task crashes mid-wait (D-18).
- `WriteToken` doc-hidden constructor + threat-model docstring (D-19).
- `ryw_inflight_cap` rename + non-FIFO doc note (D-37).
- Process-wide `set_global_ryw_inflight_cap` with two-tier acquire (D-38).
- Go binding lands `Tasks` + `Memories` adapters with `WaitForToken` + `PollForToken` + `WaitForTokenContext` (D-45).
- `wait_duration_nanos_sum` saturating u128 → u64 cast (D-51).
- Python `wait_for_token(deadline_ms=0)` non-blocking poll consistent with FFI / Node / Go (N-4).
- Go context cancellation doc contract clarified (N-11).

### FFI / cross-binding (D-20..D-23 + D-39..D-44 + D-54 + N-1 + N-10)

- cgo externs link cleanly without `dataforts` feature via `NET_ERR_FEATURE_NOT_BUILT` stubs (D-20).
- Panics across FFI caught + remapped to `NET_ERR_PANIC` (D-21).
- Vtable per-field null-check (D-22).
- `timeout_ms == 0` honored as non-blocking poll (D-23).
- `mesh_arc` drop coverage via RAII guard rather than duplicated drop-on-error (D-39).
- Node `await_tsfn_promise` applies the 30 s timeout once (was 30 s × 2 → 60 s worst case) (D-40).
- Node `DataGravityConfigJs` `*_secs / _ms` widths match the Rust + Python + Go peers (D-41).
- Python `Py<PyAny>` adapters can no longer outlive interpreter finalization (D-42).
- Python adapter `data.to_vec()` copies inside `py.detach` (D-43, N-1).
- Go `omitempty` doc note on greedy / gravity numeric fields (D-44 deferred — substrate rejects `0` for every affected field; `omitempty` is correct).
- Go `runtime.SetFinalizer` runs blocking `Close` on the GC thread — doc note rather than refactor (D-54).
- Python `atexit` drain counts drained vs. missing entries via `NET_PY_TRACE_ATEXIT` env var (N-10).

### Hygiene (D-47..D-48)

- `metrics.rs` channel-cap race doc note (D-47).
- `_force_use_hashmap` dead allow removed (D-48).

The deferred N-8 (per-peer rate-limiting of `heat:` tags) is acknowledged in D-11 and tracked for a separate slice; the auth-via-causal-claim gate forecloses the dominant attack vector today.

---

## Test hygiene

- **Lib suite at 2645+ tests** (was 2640+ at v0.14 release). 60+ net new tests across the four Rebel Yell phases + the channel-hash widening; every numbered review item ships with at least one regression where the shape made one possible. Notable additions: greedy admission + eviction unit coverage, gravity heat-counter decay + emission edge cases, blob conformance suite, RYW applied-vs-folded watermark separation, channel-hash canonical-vs-wire collision tests, RPC dispatcher race-stress + sibling-survives.
- **Cross-binding wire-format fixtures regenerate** against the 161-byte token wire form. The 159-byte token vectors under `tests/cross_lang_capability/` rename and re-encode; binding-side tests that hardcoded the 159-byte length update accordingly.
- **`cargo clippy --all-features --all-targets -D warnings` clean** across substrate + every binding crate.
- **`cargo doc --all-features --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`** — `rustdoc::broken_intra_doc_links` and `rustdoc::private_intra_doc_links` both enforce.
- **Go `go vet ./...` clean** under `CGO_ENABLED=1`; the pre-existing `testOrigin` `uint32` / `uint64` mismatch in `cortex_test.go` is fixed alongside the FFI `net_channel_hash` `u16 → u32` change.

---

## Breaking changes

### Wire format — `PermissionToken` is 161 bytes

`PermissionToken::WIRE_SIZE` grows from 159 → 161 bytes; the signed payload grows from 95 → 97 bytes. `PermissionToken::from_bytes` rejects 159-byte input as `TokenError::InvalidFormat`. Old tokens must be reissued; mixed v0.14 / v0.15 fleets cannot exchange tokens. Recommend lockstep upgrade.

### Wire format — `BlobRef` magic widens to 4 bytes

`BlobRef::MAGIC = [0xB0, 0xB1, 0xB2, 0xB3]`. Pre-v0.15 1-byte-discriminator blob refs (if any pilot deployment serialized them) are rejected on decode. Dataforts is new in v0.15, so this only matters for pre-release pilots.

### API — `ChannelHash = u32` substrate-wide

- **`ChannelName::hash()` returns `u32`** (was `u16`). New `ChannelName::wire_hash() -> u16` exposes the wire fast-path hint.
- **`channel_hash(name: &str) -> u32`** (was `u16`). New `wire_channel_hash(name: &str) -> u16`.
- **`AuthGuard::{check_fast, authorize, revoke, is_authorized}` take `ChannelHash`** (was `u16`).
- **`PermissionToken::channel_hash` is `u32`** (was `u16`); `TokenScope::with_channel`, `try_issue`, `TokenCache::{check, get}` all widen.
- **`MeshNode::register_rpc_inbound` takes `ChannelHash`** (was `u16`); `RpcInboundEvent::channel_hash` is `u32`.
- **`ChannelConfigRegistry::{get, remove, priority}` take `ChannelHash`**; new `get_by_wire_hash(u16)` for receive-side disambiguation.
- **`ChannelRegistry::get_by_wire_hash` renamed to `get_all_by_wire_hash`** and explicitly returns the full collision-bucket vector.

### FFI — `net_channel_hash` takes `uint32_t*`

```c
// v0.14
int net_channel_hash(const char* channel, uint16_t* out_hash);
// v0.15
int net_channel_hash(const char* channel, uint32_t* out_hash);
```

Go / Python / Node bindings widen their `channel_hash` / `channelHash` exports to `uint32` / `int` (u32 range) / `number` (u32 range). `TokenInfo.channel_hash` fields widen to match.

### API — Dataforts surface is new

`Redex::enable_greedy_dataforts(mesh, GreedyConfig, local_caps, IntentRegistry)`, `Redex::disable_greedy_dataforts()`, `Redex::enable_gravity_for_greedy(mesh, DataGravityPolicy)`, `Redex::disable_gravity_for_greedy()`, `BlobAdapterRegistry`, `BlobRef`, `BlobAdapter` trait, `WriteToken`, `tasks.wait_for_token` / `memories.wait_for_token` are all new in v0.15. Behind the `dataforts` Cargo feature; non-dataforts builds see typed `RedexError` stubs ("requires the `dataforts` feature; rebuild with --features dataforts") rather than a silent no-op.

### Behavioral fixes that may surface as test breakage

- **Greedy `dispatch_event` is now lock-coalesced.** Tests that asserted on the pre-fix 5-lock-per-dispatch behavior will see 1 lock in the steady state, 2 in the new-channel path.
- **`HeatRegistry` is capped at 8 K entries.** Tests that fill the registry with > 8 K entries to observe unbounded growth will see LRU eviction.
- **`should_emit_heat` returns `Skip` on near-zero `prev`.** Tests that injected `prev = 1e-300` to observe the pre-fix `inf`-prone branch will see the bootstrap arm instead.
- **`wait_for_token` returns `Err(WaitForTokenError::FoldStopped)`** when the fold task crashed mid-wait. Tests that asserted `Ok(())` against a fold-stopped adapter will see the typed error.
- **`wait_for_token(token, 0)` is a non-blocking poll** across every binding. Tests that injected 0 expecting a real `1 ms` wait will see the synchronous return.
- **`PermissionToken::from_bytes` rejects 159-byte input.** Tests that hardcoded the 159-byte wire form will see `TokenError::InvalidFormat`.

---

## How to upgrade

1. **Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.15 line.** Recompile / rebuild the binding cdylib (NAPI for Node, maturin for Python, `cargo build -p net-compute-ffi` + `-p net-rpc-ffi` for Go) with the `dataforts` Cargo feature on (pre-built release artifacts ship with the feature enabled).
2. **Channel-hash type migration.** Use `ChannelHash` (`u32`) for ACL / storage / config / RYW decisions; use `ChannelName::wire_hash()` / `wire_channel_hash()` for the 16-bit header value when constructing wire-level packets. The renames are compile errors — `cargo build` (and the binding-side TypeScript / Python static checks) drives the rewrite.
3. **Token reissue.** `PermissionToken` wire form is 161 bytes. Reissue tokens to clients; pre-v0.15 159-byte tokens are rejected on decode. The signed-payload field shifts mean old signatures don't verify against the new layout — there's no in-place upgrade.
4. **Greedy opt-in.** Channels that want greedy caching: call `Redex::enable_greedy_dataforts(mesh, GreedyConfig, local_caps, IntentRegistry)` once after constructing the `Redex` (idempotent). The runtime registers a `GreedyObserver` on the mesh's inbound dispatch; admission decisions run per inbound event without any per-channel opt-in. `Redex::disable_greedy_dataforts()` removes the observer.
5. **Gravity opt-in.** Layer gravity on top of greedy with `Redex::enable_gravity_for_greedy(mesh, DataGravityPolicy)`. The tick loop spawns automatically; tune `decay_half_life_secs`, `tick_interval_ms`, `emit_threshold_ratio`, `normalization_reference_rate` to match the deployment's read-rate skew.
6. **Blob adapter registration.** For channels that publish payloads above the inline threshold: register an adapter (`register_filesystem_blob_adapter(id, root)` for the in-tree FS adapter; `register_blob_adapter(id, instance)` for a host-language adapter), then `blob_publish(adapter_id, uri, bytes)` / `blob_resolve(blob_ref)` against the registered URI scheme.
7. **RYW opt-in.** Capture the `WriteToken` returned from every `tasks.create` / `memories.insert`; pass it to `tasks.wait_for_token(token, deadline)` before reading state that needs to reflect the write. `deadline_ms = 0` is a non-blocking poll.
8. **Operator dashboards.** `Redex::greedy_prometheus_text()` emits per-channel greedy metrics in Prometheus text format. Heat emissions ride the existing capability-announcement metrics — `dataforts_gravity_emit_total`, `dataforts_gravity_heat_registry_size`, etc.
9. **Single-binding deployments without dataforts.** Builds without the `dataforts` Cargo feature surface typed `RedexError` stubs from every `enable_*` entry point. The substrate substrate path is unchanged — RedEX, CortEX, NetDB, replication all work as in v0.14.
10. **Cross-binding wire fixtures regenerated.** If you have CI that asserts golden-vector parity against `tests/cross_lang_capability/`, the 161-byte token form means token-bearing fixtures change. The blob-ref fixtures land for the first time in v0.15.
11. **FFI consumers (C / cgo).** `net_channel_hash` takes `uint32_t*` (was `uint16_t*`). The four new Dataforts entry points (`net_redex_enable_greedy_dataforts`, `net_redex_disable_greedy_dataforts`, `net_redex_enable_gravity_for_greedy`, `net_redex_disable_gravity_for_greedy`) follow the existing `net_redex_*` shape. Without the `dataforts` feature, the symbols return `NET_ERR_FEATURE_NOT_BUILT` rather than failing to link.
12. **Mixed v0.14 / v0.15 fleets.** Replication traffic continues to work cross-version (the `SUBPROTOCOL_REDEX` wire format is unchanged). Tokens do not (161 vs 159 bytes). Recommend lockstep upgrade for any deployment using `PermissionToken`-bearing channels.

---

Released 2026-05-12.

## License

See [LICENSE](../../LICENSE).
