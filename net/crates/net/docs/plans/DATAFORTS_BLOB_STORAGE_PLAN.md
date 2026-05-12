# Dataforts Blob Storage — implementation plan (v0.2)

> Companion to [`DATAFORTS_PLAN.md`](../misc/DATAFORTS_PLAN.md). DATAFORTS_PLAN.md Phase 3 shipped in v0.15 as a `BlobRef` + `BlobAdapter` *hook* — the substrate carries the reference, the bytes live in the caller's existing storage system (S3, Ceph, IPFS, local FS). The single in-tree adapter is `FileSystemAdapter`. The plan also flags a deferred-but-named "full substrate-owned blob CAS" track. This document is that track: a **mesh-native blob store** that replaces the external-storage assumption with a content-addressed, chunked, RedEX-replicated layer composed against the v0.14 / v0.15 substrate. No new replication engine — chunks ride the existing `SUBPROTOCOL_REDEX` runtime. No S3 / IPFS / GCS integration. The mesh *is* the blob store.

## Status

**Draft v0.1.** Spec attached as [Appendix A — original Kyra spec](#appendix-a). Ships in **Rebel Yell v0.2** as Phase 3.5 (the spec is internally numbered v0.2 to mark the architectural shift from external-hook to mesh-native; the activation gate is the same as v0.15's Phase 3 — workloads whose payloads systematically exceed the inline threshold). Hard prerequisites are all shipping:

- **Phase 0** (capability-tag discovery + `blob:` tag shape) — landed v0.13.
- **Phase 2** (RedEX V2, `SUBPROTOCOL_REDEX` replication coordinator) — landed v0.14.
- **Phase 7** (5-axis `PlacementFilter`) — landed v0.13. Reused for blob placement decisions.
- **Phase 3** (`BlobRef` + `BlobAdapter` hook + `FileSystemAdapter` + per-channel registry override + adapter conformance suite) — landed v0.15. Mesh-native CAS extends the existing `BlobAdapter` trait rather than replacing it.

No backward-compat constraints: the v0.15 `BlobRef::Small` shape (4-byte magic + version + 32-byte BLAKE3 + size + URI) is forward-compatible with the v0.2 manifest tag — the `BlobRef` enum gains a `Manifest` variant; `Small` keeps its current wire form. Existing `BlobAdapter` impls keep working unchanged. The new mesh-native shape lands as an additional `MeshBlobAdapter` registered under the `mesh://` URI scheme.

## Frame

The v0.15 hook is the right shape for "the customer already has S3 / Ceph / IPFS." But every deployment that *doesn't* have an external blob system has to stand one up just to publish payloads above the inline threshold. That's the deferred track: a substrate-owned CAS that piggybacks on existing replication so operators get blob storage for free with the rest of the mesh.

Three load-bearing reasons it's mesh-native, not "ship an S3 adapter and call it done":

1. **Substrate already has all the moving parts.** RedEX V2's `ReplicationCoordinator` runs the 4-state machine, places replicas via `PlacementFilter`, heartbeats with bandwidth budgets, fails over by deterministic nearest-RTT election. A content-addressed chunk is just another RedEX file. The work is *composing* the chunking + manifest layer on top, not building distributed storage from scratch.
2. **No external dependency.** A Dataforts-enabled cluster has a working blob store the moment `enable_replication(mesh)` is called. Operators don't pick a storage backend; they pick a `replication_factor`.
3. **Unified placement + gravity model.** Blob chunks score against the same `PlacementFilter` axes as chains, attract heat under Phase 4 gravity the same way chains do, and evict under Phase 1 greedy cluster-cap pressure the same way cache files do. One set of operator knobs, not two.

## What ships

Seven things, in dependency order:

1. **`BlobRef` enum extension** — `Small` keeps its v0.15 wire form; `Manifest` lands as a new tagged variant carrying a `ChunkRef` list + an `Encoding` discriminant (`Replicated` today; `ReedSolomon { k, m }` reserved for v0.3).
2. **Chunking algorithm** — 4 MiB fixed threshold + chunk size. Below threshold: `BlobRef::Small`. Above: chunks stored independently as content-addressed RedEX files; manifest stored as a small blob (<128 bytes).
3. **`MeshBlobAdapter`** — implements `BlobAdapter` against the local mesh. `store` / `store_stream` / `fetch` / `fetch_range` / `delete` / `stat`. Routed by the `mesh://` URI scheme.
4. **`publish_with_blob` helper** — caller-level atomic that stores the bytes, waits for replica durability, then publishes the event referencing the resulting `BlobRef`. Closes the "consumer reads event before chunks are replicated" race.
5. **GC + pinning** — refcount-driven sweep over content hashes. Reference sources: RedEX chain folds, CortEX adapters indexing events, direct mesh queries for referencing events. Pins via `pin(blob_ref)` / `unpin(blob_ref)` survive GC regardless of refcount. Retention floor (default 24 h) protects newly-stored blobs against premature collection.
6. **Capability extension** — a minimal set of `cap.blob.*` + `cap.dataforts.*` capabilities lets `PlacementFilter` skip nodes that can't / shouldn't hold blobs, and lets greedy / gravity respect per-node behavioral traits + topology scope. **No new subprotocol; no new wire negotiation.** Three new fields on `CapabilitySet` + a new `Artifact::Blob` variant on `PlacementFilter`. See § 7 below.
7. **Operator surface** — Prometheus metrics (`blobs_stored_total`, `bytes_replicated_total`, `blob_replication_lag_ms`, `blob_gc_swept_total`, `blob_disk_used_bytes`, etc.) + `net blob` CLI (`ls`, `stat`, `replicas`, `gc --dry-run`, `delete`, `pin`, `unpin`) + a `dataforts:blob-storage-unhealthy` capability tag when local disk crosses 95 %.

What this plan does NOT ship (explicitly deferred):

- **Reed–Solomon encoding.** `Encoding::ReedSolomon { k, m }` is reserved on the wire so v0.3 can land it without a manifest format change, but the v0.2 implementation supports only `Encoding::Replicated`.
- **Multi-class blob tiers** (hot / cold / archive). The current model is single-tier replicated. Tiering composes against existing gravity heat counters — feasible follow-up, not v0.2 scope.
- **Trie-based manifest compression.** Manifests are flat chunk lists at the threshold. A 16 GiB blob holds 4096 chunk references (32 + 4 bytes each ≈ 144 KiB manifest) — within the small-blob path; no compression needed below the practical max-blob ceiling.
- **Delta-chunking for versioned blobs.** A v2 of a 16 GiB model with a 10 MiB delta still stores the full chunk list — content-addressed dedup catches identical chunks but doesn't catch shifted-window edits. Solved by content-defined chunking (CDC) in v0.3 if a workload demands it.
- **External adapter integration.** S3 / IPFS / GCS adapters remain valid via the v0.15 `BlobAdapter` hook surface. A deployment can mix mesh-native and external adapters per-channel via `RedexFileConfig::blob_adapter_registry`. This plan adds the mesh-native path; the external path is unchanged.
- **Cross-mesh replication of blobs.** Subnet-to-subnet blob replication composes against the existing subnet gateway machinery — out-of-scope for v0.2 unless a workload demands it.

---

## Design

### 1. `BlobRef` enum extension

```rust
pub enum Encoding {
    Replicated,
    ReedSolomon { k: u8, m: u8 },   // reserved for v0.3
}

pub struct ChunkRef {
    pub hash: [u8; 32],             // BLAKE3 of content
    pub size: u32,
}

pub enum BlobRef {
    /// Inline path — small blobs (size < 4 MiB) stored as a single
    /// content-addressed RedEX file. Wire-compatible with v0.15.
    Small { hash: [u8; 32], size: u32 },
    /// Manifest path — chunked storage. Manifest itself is a small
    /// blob; bytes are the postcard-encoded `Manifest` body below.
    Manifest {
        encoding: Encoding,
        chunks: Vec<ChunkRef>,
        size: u64,
    },
}
```

Wire encoding stays content-addressed: a `BlobRef::Manifest` serializes its body (encoding + chunks + size) and is itself stored as a `BlobRef::Small` blob. The `BlobRef` value that flows on events references the manifest's hash. Two-step resolve: `BlobRef::Manifest` → fetch manifest body → enumerate chunks → fetch chunks in parallel.

The 4-byte magic `[0xB0, 0xB1, 0xB2, 0xB3]` (v0.15) keeps its semantics. A new version byte distinguishes `Small` (`0x01`, current) from `Manifest` (`0x02`).

### 2. Chunking algorithm

Fixed 4 MiB chunks, no content-defined chunking:

- `size <= 4 MiB` → single `BlobRef::Small`. Hash = BLAKE3 of full content.
- `size > 4 MiB` → split into N = ⌈size / 4 MiB⌉ chunks. Hash each chunk independently. Pack into `BlobRef::Manifest { chunks: [ChunkRef; N], encoding: Replicated, size }`. Serialize the manifest; store its body as a `BlobRef::Small`. The flowing `BlobRef` references the manifest's hash.

Fixed chunk size across versions for determinism. Two callers chunking the same N-byte payload produce identical hash lists; identical hash lists deduplicate at the RedEX-replication layer for free.

Why 4 MiB:

- below 4 MiB the per-chunk replication overhead dominates the payload (chunk = file = `ReplicationCoordinator` worth of state).
- above 4 MiB a single chunk's tail-latency degrades partial fetches (range reads).
- 4 MiB = 1024 × `MAX_PACKET_SIZE` (8 KiB) — fits cleanly into the existing UDP fan-out window.

### 3. `MeshBlobAdapter`

Implements `BlobAdapter` against the local mesh. Registered under the `mesh://` URI scheme via `BlobAdapterRegistry::register("mesh", MeshBlobAdapter { redex, mesh })`. The `uri` parameter on `store` / `fetch` is purely cosmetic for `MeshBlobAdapter` (the content hash is the address); operators conventionally pass `mesh://<blob_hash>` for human-readable wire traces.

```rust
#[async_trait]
pub trait BlobAdapter {                            // existing v0.15 trait
    fn accepted_schemes(&self) -> &[&str];          // for MeshBlobAdapter: &["mesh"]
    async fn store(&self, uri: &str, bytes: Bytes) -> Result<BlobRef, BlobError>;
    async fn store_stream(
        &self, uri: &str, src: Pin<Box<dyn Stream<Item = Bytes> + Send>>,
    ) -> Result<BlobRef, BlobError>;
    async fn fetch(&self, uri: &str, blob: &BlobRef) -> Result<Bytes, BlobError>;
    async fn fetch_range(
        &self, uri: &str, blob: &BlobRef, start: u64, end: u64,
    ) -> Result<Bytes, BlobError>;
    async fn delete(&self, blob: &BlobRef) -> Result<(), BlobError>;   // new for v0.2
    async fn stat(&self, blob: &BlobRef) -> Result<BlobStat, BlobError>; // new for v0.2
}
```

`store(uri, bytes)`:

1. If `bytes.len() <= 4 MiB`: BLAKE3 → `hash`; store as a content-addressed RedEX file at `dataforts/blob/<hex32>`; replicate per `RedexFileConfig::replication`; return `BlobRef::Small { hash, size }`.
2. Else: split into 4 MiB chunks; for each chunk, recursively `store` (each a `BlobRef::Small`); collect `ChunkRef` list; serialize `Manifest { encoding: Replicated, chunks, size }`; recursively `store` the manifest body (also a `BlobRef::Small`); return `BlobRef::Manifest { … }`. The flowing `BlobRef` is the manifest variant; the manifest's wire ID is its own hash.

`store_stream(uri, stream)`:

- Spill to a temp file (size unknown up front).
- Compute BLAKE3 incrementally over the stream while chunking at 4 MiB boundaries.
- Each completed chunk dispatches to the replicator immediately (don't wait until the stream ends to start replicating).
- At stream-end, produce the manifest.

`fetch(uri, blob)`:

- `BlobRef::Small` → read the RedEX file at `dataforts/blob/<hex32>`. Verify BLAKE3 matches.
- `BlobRef::Manifest` → fetch manifest body; enumerate chunks; fetch chunks concurrently (bounded by `mesh_blob_fetch_concurrency`, default 8); concatenate; verify each chunk's BLAKE3.

`fetch_range(uri, blob, start, end)`:

- `BlobRef::Small` → in-memory slice.
- `BlobRef::Manifest` → compute the chunk index range `[start / 4 MiB, ⌈end / 4 MiB⌉)`; fetch only those chunks; trim the leading / trailing chunk to the requested byte range.

`delete(blob)`:

- Decrements local refcount. If refcount → 0 and retention floor passed, GC sweeps on the next cadence (see § 5).
- A `BlobRef::Manifest` delete doesn't auto-delete its chunks — chunks are independently reference-counted (other manifests may reference the same chunks). The manifest's body blob deletes; chunks delete on their own GC cycle.

`stat(blob)`:

- Returns `BlobStat { size, replicas_observed, replica_target, last_seen, encoding }`. `replicas_observed` is the count of nodes currently advertising the chunk's `causal:<hex>` tag; `replica_target` is the configured `replication_factor`.

### 4. `publish_with_blob` — transactional store-then-publish

Closes the consumer-reads-event-before-chunks-replicate race. Currently a caller has to manually sequence `blob_publish` then `publish` and hope the chunks land before the consumer pulls the event; `publish_with_blob` does the sequencing safely.

```rust
pub async fn publish_with_blob(
    mesh: &MeshNode,
    adapter_id: &str,
    uri: &str,
    bytes: impl Into<BlobInput>,        // Bytes or AsyncRead
    durability: BlobDurability,         // ReplicatedTo(n) | DurableOnLocal | BestEffort
    channel: &ChannelName,
    event: impl IntoEventPayload,
) -> Result<PublishReceipt, BlobError>;
```

Behavior:

1. `store(adapter_id, uri, bytes)` → get `BlobRef`.
2. Wait for `durability` via the same `wait_for_seq` watermark machinery that backs RYW. `ReplicatedTo(n)` waits until `n` distinct nodes have advertised the chunk's `causal:<hex>` tag (and recursively for each chunk in a manifest).
3. `mesh.publish(channel, event_with_blob_ref(event, blob_ref))` — events carry the `BlobRef` as part of their payload schema.

Variants:

- `BlobDurability::BestEffort` — return after step 1 (no wait); the existing `blob_publish` + `publish` shape today, just bundled.
- `BlobDurability::DurableOnLocal` — wait until the local RedEX file flushes to disk (matches `FsyncPolicy::Always`). No replication wait.
- `BlobDurability::ReplicatedTo(n)` — wait for n distinct replicas. Most paranoid; recommended for payment-tier traffic.

Default: `ReplicatedTo(2)` for `replication_factor >= 3` deployments, `DurableOnLocal` for `replication_factor < 3`.

### 5. GC + pinning

Refcount-driven. Reference sources:

- **RedEX chain folds** — every fold that decodes an event referencing a `BlobRef` bumps the local refcount.
- **CortEX adapters** — adapter state that holds a `BlobRef` field (e.g., a `Memories` entry with an attachment) bumps the refcount through the adapter's state-mutation methods.
- **Direct mesh queries** — `mesh.find_referencers(blob_ref)` returns the set of currently-known referencing events (best-effort; uses the capability index). Bumps a *query-time* refcount that decays.
- **Out-of-band scanner** — optional `mesh.scan_blob_references()` walks every open RedEX file and rebuilds refcounts. Run on a cadence (default 1 h) as a backstop for any refcount the live counters missed.

Sweep rules — blob is deletable iff:

- local refcount == 0 (no chain / CortEX / query holds a reference).
- age > `retention_min_age` (default 24 h — protects newly-stored blobs against premature GC under a misconfigured refcount source).
- disk pressure not critical (skip sweep when disk > 95 % to avoid making a bad-day worse).
- not pinned.

Pinning:

```rust
mesh.pin(blob_ref)?;
mesh.unpin(blob_ref)?;
```

Pins survive GC regardless of refcount. Operator escape hatch for "this blob must not disappear until I say so" (audit logs, regulatory holds). Pins are local to the node — a pin on node A doesn't keep the blob alive on node B; node B's GC runs against its own refcount + retention floor + disk pressure.

### 6. Operator surface

**Prometheus metrics** (per adapter + per node):

- `dataforts_blobs_stored_total{adapter}` — count of `store` / `store_stream` returns.
- `dataforts_blobs_fetched_total{adapter}` — count of `fetch` / `fetch_range` returns.
- `dataforts_blob_bytes_stored_total{adapter}` — bytes stored locally.
- `dataforts_blob_bytes_replicated_total{adapter}` — bytes shipped via the replication subprotocol.
- `dataforts_blob_replication_lag_ms{adapter,channel}` — heartbeat lag against the chunk's RedEX file.
- `dataforts_blob_gc_swept_total{adapter}` — count of blobs GC removed.
- `dataforts_blob_gc_pending_total{adapter}` — current count waiting on retention floor.
- `dataforts_blob_disk_used_bytes{adapter}` — bytes on local disk.
- `dataforts_blob_disk_capacity_bytes{adapter}` — operator-configured cap.

**CLI** (`net blob …`):

```text
net blob ls                       # list locally-stored blob hashes
net blob stat <ref>               # size, replicas, encoding, age
net blob replicas <ref>           # node IDs currently advertising the blob's causal: tag
net blob gc --dry-run             # show what would be swept
net blob delete <ref>             # explicit delete (decrements refcount)
net blob pin <ref>                # protect from GC
net blob unpin <ref>              # release protection
```

**Health gates.** When local disk > 95 %, the node advertises a `dataforts:blob-storage-unhealthy` capability tag. The `PlacementFilter` skips unhealthy nodes when placing new chunks; chain-level mesh ops are unaffected. The node clears the tag when disk drops back below 85 % (hysteresis).

### 7. Capability extension

The substrate must know **which nodes can hold blobs at all** and **how greedy / gravity should behave per node**. Both are per-node behavioral traits, not cluster-wide flags — a mesh routinely mixes compute-only nodes, storage-only nodes, and hybrids; a multi-region deployment routinely wants greedy bounded to a region. The right place for these is the existing `CapabilitySet`; the substrate already carries them across the mesh via the capability index and the `causal:` tag propagation path.

**Three load-bearing reasons this is a capability, not a config flag:**

1. **Mixed node roles.** A 50-node cluster running 10 compute-heavy nodes (no blob storage), 30 hybrid nodes, and 10 storage-heavy nodes can't be expressed as a global flag. Two nodes in the same mesh participate differently in placement, greedy, and gravity decisions.
2. **No new wire protocol.** The capability index, the proximity graph, hierarchical summarization, and `find_nodes_by_filter` all already propagate per-node traits. Adding three new capability fields rides those primitives at no per-packet cost.
3. **No multi-capability handshake.** Nodes that don't know about blob storage advertise `blob.storage = false` (default); they participate in chain-level ops unchanged. There's nothing to negotiate.

The **minimal correct set**:

```rust
pub struct CapabilitySet {
    // ... existing axes (hardware / software / devices / dataforts) ...
    pub blob: BlobCapability,
    pub dataforts_greedy: GreedyCapability,
    pub dataforts_gravity: GravityCapability,
}

pub struct BlobCapability {
    /// Does this node participate in blob storage at all?
    pub storage: bool,
    /// Operator-configured cap for blob disk (separate from RedEX disk).
    pub disk_total_gb: u64,
    /// Updated on heartbeat (default 5 s cadence). Drives placement
    /// scoring + the `blob-storage-unhealthy` health gate.
    pub disk_free_gb: u64,
    /// Reserved for v0.3 tiering. `None` in v0.2.
    pub class: Option<BlobClass>,    // Hot / Warm / Cold / Archive
}

pub struct GreedyCapability {
    /// Does this node act as a greedy puller at all?
    pub enabled: bool,
    /// Topology boundary greedy is allowed to cross.
    pub scope: TopologyScope,        // Node / Zone / Region / Mesh
    /// Soft-preference weight (0–255). 0 = greedy disabled even
    /// when `enabled = true`; high = prefer near peers; low =
    /// allow farther peers under cost-tolerant policy.
    pub proximity: u8,
}

pub struct GravityCapability {
    /// Does this node participate in heat-driven migration?
    pub enabled: bool,
    pub scope: TopologyScope,
    pub proximity: u8,
}

pub enum TopologyScope {
    /// Migrate / pull only on the same node (debug / single-node).
    Node,
    /// Same failure-domain zone (rack / power domain).
    Zone,
    /// Same region (typically same datacenter / cloud region).
    Region,
    /// Whole mesh (no scope constraint).
    Mesh,
}
```

**`scope` vs. `proximity` — two control planes:**

- `scope` is a **hard boundary**. `GreedyCapability::scope == Zone` means the node never pulls blobs originating outside its zone, no matter how attractive the heat score. `gravity_scope == Region` means a hot blob never drifts across regions, even if a higher-RTT node is the heat source. Hard cuts off the worst failure modes (cross-WAN egress costs, cross-region partition risk, compliance boundaries).
- `proximity` is a **soft preference weight**. `greedy_proximity = 200` says "strongly prefer near peers"; `= 64` says "tolerate farther peers." `0` disables the policy even when `enabled = true`. Soft drives the score-based placement decisions inside the allowed scope.

Both are needed because they answer different questions: scope answers *"is this peer eligible at all?"*, proximity answers *"among eligible peers, which to pick?"*.

**`PlacementFilter::Artifact::Blob`:**

```rust
pub enum Artifact {
    Chain { origin_hash: u64, capabilities: Arc<CapabilitySet> },
    Replica { channel: ChannelName, capabilities: Arc<CapabilitySet> },
    Daemon { daemon_id: String, required: Vec<Tag>, optional: Vec<Tag> },
    // v0.2 — new:
    Blob {
        blob_hash: [u8; 32],
        size_bytes: u64,
        encoding: Encoding,
        capabilities: Arc<CapabilitySet>,
    },
}
```

`StandardPlacement::placement_score(&Artifact::Blob, node)` factors in:

- `node.blob.storage == true` — gate; non-storage nodes score 0.
- `node.blob.disk_free_gb >= size_bytes / 1 GiB + slack` — disk-pressure gate.
- `dataforts:blob-storage-unhealthy` tag absent — health gate.
- failure-domain tags (rack / zone / region) for anti-affinity with existing replicas.
- `proximity` (RTT to the publisher) — soft-weighted.

The same `StandardPlacement` config that scores chains scores blobs. Operator knobs (`scope_filter`, `proximity_max_rtt`, `intent_match`, `colocation_policy`, `resource_axis`, `metadata_keys`) all apply unchanged.

**Update frequency.** `disk_free_gb` updates on the heartbeat cadence (default 5 s); `storage` / `enabled` flags update only on operator action (no need to re-advertise every tick). The heartbeat-frequency update for `disk_free_gb` is the same rate the proximity graph already runs; no new traffic.

**Cargo feature gate.** The capability extensions land behind the existing `dataforts` feature. Builds without the feature serialize the new fields as defaults (`storage: false`, `enabled: false`, `scope: Mesh`, `proximity: 0`) — wire-compatible with v0.15 nodes that don't know the fields exist, since `CapabilitySet` already serializes as a postcard struct that tolerates unknown trailing fields.

---

## Dataforts integration rules

Six integration points; each is a contract the implementation must hold.

### G-1 — Greedy (Phase 1)

Greedy pulls **only blobs referenced by artifacts it already pulled**, not arbitrary blobs. The rule:

- When the greedy runtime admits a chain into the cache and the chain's events carry `BlobRef`s, greedy *additionally* pulls those blobs.
- Greedy does NOT speculatively pull blobs on the basis of `blob:<hex>` capability tags alone — that path would explode disk usage on referenced-once-per-million-events data.
- Greedy DOES weight blob admission by the parent chain's heat (Phase 4) and proximity (Phase 7).

**Capability gating** (new in v0.2):

- A node with `dataforts_greedy.enabled = false` never speculatively pulls blobs, no matter what its parent chain admits.
- A node with `blob.storage = false` never receives blob replicas at all (placement skips it).
- A node with `dataforts_greedy.proximity = 0` is functionally greedy-disabled for blobs even when `enabled = true`.
- `dataforts_greedy.scope` is a hard boundary — `scope == Zone` means greedy doesn't pull blobs whose publisher is in a different zone, regardless of heat score.

Counters: `dataforts_greedy_blob_pulls_admitted_total` / `…_rejected_total{reason}` where `reason ∈ { Scope, Proximity, NoStorageCap, GreedyDisabled, DiskPressure }`.

### G-2 — Gravity (Phase 4)

Heat applies to blobs the same way it applies to chains:

- Frequent `fetch` / `fetch_range` calls on a blob bump its `HeatCounter`.
- Threshold-crossing emissions stamp `heat:<hex>=<rate>` onto the blob's existing capability advertisement (same code path as chain heat — blobs ride the `causal:` shape).
- Hot blobs attract additional replicas; cold blobs decay and may reduce replica count to `replication_factor - 1` under disk pressure.

**Capability gating** (new in v0.2):

- A node with `dataforts_gravity.enabled = false` doesn't pull migrating blobs, even if its heat score would otherwise win the placement.
- `dataforts_gravity.scope` bounds the migration radius — `scope == Region` means a hot blob never drifts out of its source region, even when a higher-RTT node is the heat source. Multi-region deployments configure `scope = Region` by default; multi-cloud configures `scope = Zone` to keep migration off the WAN.
- `dataforts_gravity.proximity` weights the score-based migration decision inside the allowed scope; `0` disables migration on this node.
- Gravity-side replica reduction (cold blobs trim to `replication_factor - 1`) respects the `blob.storage` gate — a node that doesn't carry blobs at all isn't a candidate to *lose* one either.

### G-3 — Migration

Blob replicas migrate under Phase 4 gravity exactly like chain replicas. No special cases. The same `ReplicationCoordinator` 4-state machine drives placement transitions; replica withdrawal under disk pressure follows the existing `UnderCapacity::Withdraw` path.

### G-4 — Placement

Blob placement uses the exact same primitives as chain placement:

- `PlacementStrategy::Standard` defers to the 5-axis `PlacementFilter` (scope + proximity + capability-preference + colocation + storage-cap).
- `PlacementStrategy::Pinned([NodeId])` skips the filter — but still gates on `blob.storage = true`; pinning to a non-storage node returns a typed error at placement time, not silent corruption.
- `PlacementStrategy::ColocationStrict` requires the blob to land on a node already carrying its colocation target AND `blob.storage = true`.

`PlacementFilter::placement_score(&Artifact::Blob, node)` factors in `node.blob.storage`, `node.blob.disk_free_gb`, the `dataforts:blob-storage-unhealthy` health tag, failure-domain anti-affinity, and proximity (see § 7 above for the full scoring formula).

Unified placement model. One operator knob set, not two.

### G-5 — Read-your-writes (Phase 5)

`publish_with_blob` extends the RYW machinery: the `PublishReceipt` carries a `WriteToken` whose `seq` is the publish-event's seq, but whose origin-binding asserts replica-durability via the same `applied_through_seq` watermark used by RYW. A consumer calling `tasks.wait_for_token(token, deadline)` (or `wait_for_blob_token`) blocks until both the event has folded *and* the blob's chunks have replicated to the configured durability.

### G-6 — Auth

`pin` / `unpin` / `delete` require an `AuthGuard::is_authorized_full(origin, channel)` check against the chain that originally published the blob. A peer that can publish on channel X can pin / delete the blobs referenced by chain X's events; nothing else.

---

## Consistency / durability semantics

Documented matrix — what the spec promises and what it explicitly doesn't.

### W-1 — Write semantics

When `store(uri, bytes)` returns `BlobRef`:

- blob is persisted on the local node.
- replication is in-progress (the chunks' RedEX `ReplicationCoordinator`s have spawned, sync requests are in flight).
- the blob may NOT yet be readable from other nodes.
- durability depends on `RedexFileConfig::replication.factor`.

For durability guarantees beyond local-write, use `publish_with_blob` with `BlobDurability::ReplicatedTo(n)`.

### W-2 — Durability guarantee

For `replication_factor = N`:

- survives loss of N–1 nodes (matches RedEX replication).
- correlated failures (power domain / rack / region) depend on placement tags — operators configure `PlacementFilter` axes to bound the correlated-failure radius.

### W-3 — Read consistency

- **local read**: immediate (blob is in the local RedEX file).
- **remote read**: after first replica arrives (consumer's local mesh sees the chunk via the standard `SUBPROTOCOL_REDEX` heartbeat round).
- **worst-case latency**: one heartbeat round (500 ms default).
- **eventual consistency**: guaranteed by the RedEX replication coordinator; same semantics as chain replication.

### W-4 — Partition semantics

- Both sides of a partition may write blobs.
- Manifests remain causal because of content-addressing — a write on side A and a different write on side B produce different `BlobRef` hashes; there's no "same blob, conflicting content" possibility.
- Conflict resolution = content-addressing. **No merges needed.** If two partitions wrote different content, they produced different blobs; the chain events that reference them retain their distinct references.

This is the load-bearing property of content-addressed storage: it makes the partition story trivial.

---

## Test strategy

Five layers, each with explicit DST scope where determinism is gate-able.

### T-1 — Unit (pure-logic)

- `BlobRef::Small ↔ Manifest` round-trip; manifest enumeration; chunk-index range math.
- Chunking algorithm — fixed boundaries; idempotency on identical input.
- `fetch_range` translation — every range maps to the right chunk indices and trim offsets.
- GC sweep rule — every combination of (refcount, age, pinned, disk-pressure) produces the expected sweep verdict.
- `publish_with_blob` durability waiting — `ReplicatedTo(n)` blocks until `n` advertise; tests force the count via mock `causal:<hex>` advertisers.

### T-2 — Integration (e2e on multi-tokio-thread)

- Two-node store + replicate + remote-fetch.
- Three-node fanout: leader stores, two replicas pull; node-loss handover; replica rejoin.
- `publish_with_blob` end-to-end: consumer subscribes, producer stores + publishes, consumer fetches before / after / during replica arrival.
- `fetch_range` partial-fetch correctness on a 100 MiB chunked blob.
- GC under disk pressure; pin survives a forced sweep.

### T-3 — DST (deterministic-simulation)

- Reuse the `redex_replication_dst.rs` harness; add blob scenarios (multi-chunk store under partition, manifest-then-chunks ordering, retention-floor sweep timing).
- Chunking idempotency under racing concurrent stores on the same content (both should resolve to the same `BlobRef`; the replication coordinator dedupes by content hash).
- Partition-heal divergence-freedom — write the same logical content on both sides under a different `uri`; both produce the same `BlobRef` (because content-addressed); replicas converge.
- **Mixed-capability cluster scenarios**: 3-node cluster with one compute-only (`blob.storage = false`), one storage-only, one hybrid — assert placement never lands chunks on the compute-only node, gravity never migrates toward it, and greedy never pulls toward it. Assert the *negative*: a compute-only node observing a `causal:<blob_hex>` advertisement does NOT bump its `HeatCounter`.
- **Cross-scope enforcement**: 4-node cluster split across two zones (`zone:a` and `zone:b`) with `gravity_scope = Zone`. Heat-driven migration must stay within zones; cross-zone drift never happens regardless of heat differential.
- **Proximity-weighted convergence**: 3-node cluster with mixed `proximity` weights (255 / 128 / 0). Hot blob originating from node A must converge toward the high-proximity node first; the zero-proximity node never receives speculative pulls even when it's the closest peer.

### T-4 — Conformance

Extend the v0.15 `BlobAdapter` conformance suite (in-tree at `dataforts/blob/conformance.rs`) with mesh-native test cases. The same suite that gates `FileSystemAdapter` gates `MeshBlobAdapter`: idempotency, hash-mismatch rejection, range-past-end, cross-blob isolation, random-ghost reads, plus four new cases:

- streaming-store with unknown length (idempotency on retry).
- partial chunk replication (fetch with N-of-M replicas reachable).
- GC sweep skips pinned + retention-floor-protected blobs.
- `BlobDurability::ReplicatedTo(n)` actually waits the right number of acks.

### T-5 — Cross-binding

Wire-format parity across Rust / Python / Node / Go / C. The `BlobRef::Manifest` encoding is a postcard blob; every binding round-trips the same `BlobRef` through the FFI without value drift. Cross-binding fixtures land under `tests/cross_lang_capability/blob_manifest_*.json`.

---

## Open design questions to lock before implementation

These need a ratify-or-revise decision before PRs start.

1. **Chunk size — fixed 4 MiB or configurable?** Spec says fixed for determinism. A configurable knob means two callers can produce different `BlobRef`s for the same content. Recommended: fixed at 4 MiB; revisit only if a workload demonstrates that 4 MiB is materially wrong for it (10× over- or under-shoot).
2. **Manifest format — postcard or hand-rolled byte-stable?** Postcard is the v0.14 / v0.15 convention for cross-binding state. Hand-rolled would let us version the manifest byte layout independently of postcard's schema-evolution rules. Recommended: postcard with a 1-byte version prefix (consistent with `BlobRef::Small` shape).
3. **`stat` against a not-locally-present blob — best-effort or strict?** Strict (`BlobError::NotFound`) is the conservative answer; best-effort (return `replicas_observed: 0`) is the operator-friendly one for "is anyone holding this blob?" queries. Recommended: add `mesh.stat_remote(blob_ref)` for the best-effort flavor; keep `stat` strict-local.
4. **`delete` semantics — recursive or surface-only?** A `BlobRef::Manifest` delete that auto-removes its chunks is convenient but breaks the dedup property (other manifests may reference the same chunks). Recommended: surface-only delete (manifest body removed; chunks deleted on their own GC cycle).
5. **`publish_with_blob` failure mode — atomic rollback or surfaced error?** If step 2 (durability wait) times out, do we roll back the stored chunks? Recommended: no rollback. The chunks are content-addressed; an aborted publish leaves the blob in place. Future publishers with the same content will dedupe.
6. **GC out-of-band scanner cadence.** Default 1 h is a guess; a node holding 10 M blobs might want less frequent sweeps. Recommended: 1 h default, `RedexFileConfig::blob_gc_scan_interval` operator override.
7. **`disk_free_gb` heartbeat cadence.** 5 s default matches the proximity-graph heartbeat. Faster updates give placement decisions fresher data but increase advertisement volume; slower updates risk placing a chunk on a node that just filled up. Recommended: 5 s default, `BlobCapability::disk_free_update_interval` operator override.
8. **Greedy / gravity capability default at first boot.** A fresh `dataforts`-enabled node could default to `enabled = true` (opt-out) or `enabled = false` (opt-in). Opt-out is friendlier for single-cluster deploys; opt-in is safer for multi-region pilots. Recommended: opt-out (`enabled = true`, `scope = Mesh`, `proximity = 128`) — match the v0.15 behavior so existing deployments don't see a policy change when the v0.2 capability fields land.
9. **Capability scope granularity.** `TopologyScope { Node, Zone, Region, Mesh }` is intentionally coarse — finer granularity (e.g. `Rack`, `AvailabilityZone`) would require an explicit failure-domain hierarchy the substrate doesn't track today. Recommended: ship the four-variant enum; document `Zone` as "operator-defined failure boundary, typically rack-level" and let the operator's `scope:<label>` capability tags supply the actual mapping.

Lock each via a single-line decision in the v0.2 release notes when the implementation lands.

---

## Risks

- **Refcount divergence.** The four refcount sources (chain folds, CortEX, query, out-of-band scanner) can drift if any source misses a reference. Mitigation: out-of-band scanner is the backstop; retention floor is the airlock; pinning is the operator escape hatch.
- **Manifest body GC race.** A small-blob `Manifest` body could be GC'd while another node still holds a reference. Mitigation: manifest bodies refcount the same way payloads do; the chain event referencing the parent `BlobRef::Manifest` bumps both the manifest's refcount AND each chunk's refcount on fold.
- **Streaming store + replication budget.** Multi-GB streams generate sustained replication traffic; pre-existing `replication_budget_fraction` bounds it, but a single 64 GiB stream still saturates the configured budget for tens of minutes. Mitigation: documented operator guidance; surface budget-engagement counter.
- **Chunk-index integer overflow.** 64 MiB / 4 MiB = 16 chunks. 16 GiB / 4 MiB = 4096 chunks. 1 TiB / 4 MiB = 262 144 chunks. `Vec<ChunkRef>` with 144-byte elements (rough) holds ~37 MiB for a 1 TiB blob's manifest — within practical memory but on the edge of the "small blob" path. Mitigation: `BlobRef::MAX_SIZE` default 16 GiB; operators lift the cap explicitly if needed.
- **No CDC (content-defined chunking)** means a 1-byte edit at the start of a 16 GiB file shifts every chunk boundary and produces an entirely new chunk set — zero dedup. Mitigation: documented as a v0.3 follow-up; v0.2 ships with fixed chunking and that's the trade-off.
- **Cross-binding stat semantics.** A Python caller calling `stat()` against a `BlobRef::Manifest` expects... what? Total size, or per-chunk array? Mitigation: pin the `BlobStat` shape in the conformance suite; Python returns a typed `BlobStat` dataclass with consistent fields across bindings.

---

## Effort

Five PRs in dependency order. Each is self-contained and can land independently behind the `dataforts` Cargo feature.

### PR-1 — `BlobRef::Manifest` + chunking pure-logic (1 week)

- `BlobRef::Manifest` variant + encode/decode round-trip.
- Chunking algorithm + fixed-threshold split.
- `ChunkRef` type + manifest body encoding (postcard + version byte).
- Unit tests for chunk-index range math + idempotency.

### PR-2 — `MeshBlobAdapter` + capability extension (2 weeks)

- `MeshBlobAdapter` impl against `Redex` + `MeshNode`.
- `store` / `store_stream` / `fetch` / `fetch_range` / `delete` / `stat`.
- `BlobStat` shape across bindings.
- Reuses RedEX replication; **no new replication code** lands in this PR (gating discipline).
- `BlobCapability` / `GreedyCapability` / `GravityCapability` / `TopologyScope` types land on `CapabilitySet`; postcard wire-compat with v0.15 nodes via tolerated trailing fields.
- `Artifact::Blob` variant on `PlacementFilter`; `StandardPlacement::placement_score(&Artifact::Blob, node)` factors in `blob.storage` / `disk_free_gb` / health tag / failure-domain tags / proximity.
- Conformance suite extension (T-4).

### PR-3 — `publish_with_blob` + durability (1 week)

- `BlobDurability` enum + waiting semantics.
- Atomicity contract: store → wait → publish, with documented failure modes.
- Integration tests against multi-node setup.

### PR-4 — GC + pinning + operator surface (2 weeks)

- Refcount sources (chain folds, CortEX, query, scanner).
- Sweep rules + retention floor + pin / unpin.
- `net blob` CLI.
- Prometheus metrics.
- Health gate (`blob-storage-unhealthy` capability tag).

### PR-5 — Dataforts integration + DST (1–2 weeks)

- Greedy / gravity / placement / migration / RYW / auth wiring (G-1 through G-6).
- Capability-gated admission paths — greedy + gravity respect `enabled` / `scope` / `proximity` per node.
- DST harness extensions (T-3) including mixed-capability cluster scenarios (compute-only nodes refusing blob replicas, cross-region scope enforcement, proximity-weighted convergence).
- Cross-binding fixtures (T-5).

**Total: 7–8 weeks focused.** Parallelism opportunities: PR-2 and PR-4's CLI work; PR-3 and PR-5's DST work. Worst-case sequential: 8 weeks.

---

## Shipping status

| PR     | Commit       | Scope shipped                                                                                                   |
|--------|--------------|-----------------------------------------------------------------------------------------------------------------|
| PR-1   | `6d824d11`   | `BlobRef::Manifest` variant + chunking pure-logic.                                                              |
| PR-2a  | `cd14ffe3`   | `MeshBlobAdapter` + `BlobAdapter::{delete,stat}` trait methods + `BlobStat` shape.                              |
| PR-2b  | `d92de2b4`   | `BlobCapability` / `GreedyCapability` / `GravityCapability` / `TopologyScope` + `Artifact::Blob` placement.     |
| PR-3   | `f49e7dd9`   | `publish_with_blob` + `BlobDurability` (BestEffort / DurableOnLocal / ReplicatedTo).                            |
| PR-4a  | `a75b0df6`   | `BlobRefcountTable` + GC sweep + retention floor + pin/unpin + `BlobMetrics` + health gate hysteresis.          |
| PR-5a  | `74dcaee8`   | Decision primitives: `should_pull_blob` (G-1), `should_migrate_blob_to` (G-2/G-3), `auth_allows_blob_op` (G-6). |
| PR-5b  | `585a78ef`   | G-6 wiring: `MeshBlobAdapter::with_auth_guard` + `pin_authorized` / `unpin_authorized` / `delete_chunk_authorized`. |
| PR-5c  | `36ed5656`   | G-1 wiring: `dispatch_event` runs `should_pull_blob` after admission + bumps blob-pull counter family.          |
| PR-5d  | `6497ea81`   | G-1 e2e (T-3): admit on participating node, no_storage reject on compute-only, greedy_disabled reject.          |
| PR-5e  | `762a06c4`   | G-1 e2e cross-scope admit path (Zone-local, Mesh-publisher); reject path was deferred pending the chain_caps refactor below — now unblocked. |
| PR-5f  | `9ee20ffd`   | Plan doc shipping status + initial deferred-items inventory.                                                    |
| PR-5g  | `d16e31e5`   | `CapabilityIndex::get_by_origin_hash` side index + chain_caps lookup refactor; deterministic G-1 cross-scope reject e2e. |
| PR-5h  | `cc0fe746`   | Greedy as chain-fold refcount source: `set_blob_refcount_table` + `incr` on G-1 admit + `decr` on cache eviction. |
| PR-5i  | `3e7fac67`   | `BlobAdapter::prefetch` trait method + `MeshBlobAdapter` opens chunk channels on prefetch; greedy spawns prefetch on G-1 admit + counter family. |
| PR-5k  | `7456596a`   | Typed capability setters: `BlobCapability::write_into` + `GreedyCapability::write_into` + `GravityCapability::write_into` + matching `CapabilitySet::with_*` builders. |
| PR-5j-a/b | `d005d1ce` | `BlobHeatRegistry` (mirrors `HeatRegistry` keyed on `[u8;32]`) + `MeshBlobAdapter::with_blob_heat` bumping on `fetch`/`fetch_range`. |
| PR-5j-c | `dbda7208` | `BlobHeatSink` trait + `MeshNode::announce_blob_heat`/`withdraw_blob_heat`/`announce_blob_heat_batch` + `MeshBlobAdapter::tick_blob_heat` emission loop. |
| PR-5j-d | `49e9f41d` | `parse_blob_heat_tag` + `BlobMigrationController` + `drive_blob_migration_tick` async helper; closes the Phase-4b gravity migration loop. |

### Still deferred — items that warrant their own design step

- **Cross-binding fixtures (T-5).** Python / Node / Go binding wrappers exist for the v0.15 `BlobAdapter` shape but not for the v0.2 `MeshBlobAdapter` + `publish_with_blob` + `BlobRefcountTable` + `BlobMetrics` + `prefetch` + migration-controller surfaces. Useful follow-up; not load-bearing for the substrate. Likely shipped per-binding (Python first, given the project's primary FFI consumer) rather than as one bulk PR — each language's surface is a few hundred lines and has its own idiomatic shape.
- **`net blob` CLI.** Operator surface deferred from PR-4a's scope. No existing CLI binary in `net/crates/net` today (no `[[bin]]` target + no `clap`/`argh` dep), so a `net blob` tool would start with bin scaffolding + an IPC/connection model to talk to a running `MeshNode`. Useful follow-up but needs design context (operator vs developer tool, daemon vs library mode) before scope crystallizes.
- **Multi-node prefetch e2e.** PR-5i's prefetch tests use a `RecorderAdapter` mock at the unit level; PR-5j-d's migration tests use the same mock against a populated `CapabilityIndex`. A 3-node DST proving A publishes + advertises → B admits via G-1 + calls `prefetch` → C is the actual chunk holder + replicates to B is a useful follow-up but requires a 3-node harness the current 2-node `dataforts_greedy_e2e.rs` doesn't express. Same harness gap blocks an end-to-end gravity-migration DST: A publishes → A's fetches bump heat → A's gravity tick emits `heat:blob:` tags → B observes the tags via gossip → B's migration controller calls prefetch → B's chunk channels populate.
- **Manifest-aware migration.** `parse_blob_heat_tag` surfaces a single 32-byte hash; the migration controller builds a `BlobRef::Small` from `(hash, size)` and prefetches that single chunk. Recursive per-chunk migration of `BlobRef::Manifest` blobs needs a side index relating the manifest hash to its chunk list — either an `EventMeta` projection the publisher maintains or a separate `manifest:<hex>:chunks=<list>` capability tag family. Useful refinement once a workload demonstrates manifest-sized blobs dominating the migration traffic.

---

## Activation gate

A workload demonstrating *systematic* publishes above the inline threshold where the v0.15 external-hook surface is the wrong shape. Realistic triggers:

- AI model artifacts (10 GiB – 1 TiB) where the deployment doesn't have S3 / IPFS / Ceph available.
- High-resolution sensor data (multi-MB images / point clouds) where a per-deployment external blob store would be operational overhead.
- Pilot deployments where "stand up a Net cluster, get blob storage for free" is the operator pitch.

If the workload comes via "we have S3 and want to keep using it" — v0.15's `BlobAdapter` hook is the right shape, no mesh-native CAS needed. If it comes via "we don't want to operate two storage systems" — Dataforts Blob is the answer.

---

## See also

- [`DATAFORTS_PLAN.md`](../misc/DATAFORTS_PLAN.md) — the seven-phase plan including v0.15 Phase 3's external-hook shape.
- [`DATAFORTS_FEATURES.md`](../misc/DATAFORTS_FEATURES.md) — the audit; mentions "deferred-but-named: full substrate-owned blob CAS" — this plan is that track.
- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — the v0.14 replication runtime that blob chunks ride on. No replication-side changes needed for v0.2 blob.
- [`RELEASE_v0.15_REBEL_YELL.md`](../releases/RELEASE_v0.15_REBEL_YELL.md) § Phase 3 — what shipped as the external-hook surface and what this plan extends.

---

## Appendix A — original spec

The plan above is derived from Kyra's "Dataforts Blob Storage Spec (v0.1)" — internally labeled v0.1 by Kyra, surfacing under the v0.2 blob-track shipping label in this plan (the spec's wire format slots into Dataforts Phase 3 as the next iteration on top of v0.15's external-hook shape). Reproduced verbatim below for traceability; section numbers in the body of this plan map to the spec's sections via the following crosswalk:

| Plan §       | Spec §                              |
|--------------|-------------------------------------|
| § 1          | Spec § 2 — `BlobRef` structure       |
| § 2          | Spec § 5 — chunking algorithm        |
| § 3          | Spec § 3 — adapter interface         |
| § 4          | Spec § 7 — transactional publish     |
| § 5          | Spec § 8 — garbage collection        |
| § 6          | Spec § 11 — operational surface      |
| § G-1..G-6   | Spec § 9 — Dataforts integration     |
| § W-1..W-4   | Spec § 10 — consistency / durability |
| § T-1..T-5   | (no spec § — planning addition)      |
| § PR-1..PR-5 | (no spec § — planning addition)      |

Kyra's spec referred to the layer as "MeshNative Blob Storage" / "MeshOS"; the consistent project naming is **Dataforts**.

```text
DATAFORTS BLOB STORAGE SPEC (v0.1)
Status: Draft
Backward Compat: N/A (no users yet)
Future-proof: Yes (erasure coding reserved)

1. Overview
The Dataforts Blob Store provides:
- content-addressed blob storage
- chunking of large objects
- mesh-native replication
- range reads + streaming IO
- garbage collection
- publish-with-blob atomics
- integration with Dataforts (greedy + gravity)
- operator introspection + metrics
S3/IPFS/GCS/etc. are not required or integrated.
Blob storage runs entirely on the mesh substrate.

2. BlobRef Structure
enum Encoding {
    Replicated,
    ReedSolomon { k: u8, m: u8 },   // reserved for future v2
}
struct ChunkRef {
    hash: Hash256,                  // blake3 of content
    size: u32,
}
enum BlobRef {
    Small { hash: Hash256, size: u32 },
    Manifest {
        encoding: Encoding,
        chunks: Vec<ChunkRef>,
        size: u64,
    }
}
- Small: blobs < 4 MiB (non-chunked)
- Manifest: maps to chunk list and encoding type
- Encoding tag reserved for future erasure coding

3. Adapter Interface
trait BlobAdapter {
    async fn store(&self, uri: &str, bytes: Bytes) -> BlobRef;
    async fn store_stream(&self, uri: &str, stream: impl AsyncRead) -> BlobRef;
    async fn fetch(&self, uri: &str, ref_: &BlobRef) -> Bytes;
    async fn fetch_range(&self, uri: &str, ref_: &BlobRef, start: u64, end: u64) -> Bytes;
    async fn delete(&self, ref_: &BlobRef);
    async fn stat(&self, ref_: &BlobRef) -> BlobStat;
}
Non-negotiable requirements:
- store_stream and fetch_range are required for large blobs
- fetch is sugar for fetch_range(0..size)
- store / store_stream are content-addressed
- All operations return BlobRef deterministically

4. Replication Semantics (Using RedEX)
All blobs (small + chunked chunks) are replicated via the RedEX replication layer:
replication_factor: u8         // default: 3
placement: PlacementStrategy   // reuses chain placement logic
Chunk replication is:
- identical to chain replication
- uses the same state machine: Idle/Replica/Candidate/Leader
- uses the same heartbeats
- subject to the same bandwidth + replication budgets
There is no new replication engine. This is simply content-addressed RedEX.

5. Chunking Algorithm
Threshold: 4 MiB.
If blob size > threshold:
- split into 4MiB chunks
- compute blake3 hash per chunk
- create BlobRef::Manifest with chunk list
- store chunks independently
- store manifest as small blob (<128 bytes)
If <= threshold:
- small blob path (single BlobRef::Small)
Chunk size fixed (4MiB) across versions for determinism.

6. Streaming IO
store_stream(uri, AsyncRead):
- Accept input as stream
- Spill to temp file
- Chunk at 4MiB boundaries
- Upload chunk-by-chunk using RedEX replication
- Produce BlobRef manifest
fetch_range(uri, start..end):
- Translate range to chunk indices
- Fetch only relevant chunks
- Return slices concatenated in order
- Supports large video, model weights, data processing

7. Transactional Publish
Adds a new helper at mesh client level:
publish_with_blob(
    adapter_name: &str,
    uri: &str,
    bytes: impl Into<AsyncRead>,
    durability: BlobDurability,
    event: Event,
) -> PublishReceipt
Behavior:
1. store -> get BlobRef
2. wait for replica durability (RedEX ack)
3. publish event referencing the BlobRef atomically
This prevents races:
- consumer sees event before blob is replicated
- classic "missing blob on first read" bugs

8. Garbage Collection
GC is required for correctness.
8.1 Refcount Sources
- RedEX chain folds
- CortEX indexing
- Direct mesh queries for referencing events
- Optional out-of-band scanner
8.2 Sweep Rules
Blob is deletable when:
- refcount == 0
- age > retention_min_age (e.g. 24h)
- disk pressure not critical
8.3 Pinning
pin(ref); unpin(ref)
Pins survive GC regardless of refcount.

9. Dataforts Integration
Blobs must integrate with:
- P1 (Greedy)
- P4 (Gravity)
- P7 (PlacementFilter)
- Rebel-Yell P1/P4
9.1 Greedy: Greedy pulls only blobs referenced by artifacts it already pulled. Not arbitrary blobs.
9.2 Gravity: frequent reads raise heat; hot blobs gain replicas / migrate closer; cold blobs decay.
9.3 Migration: Blob replicas migrate under P4 exactly like chain replicas.
9.4 Placement: Blob placement uses the exact same PlacementStrategy / tags / cardinality cache / scoring.

10. Consistency / Durability Semantics
10.1 Write Semantics
When store() returns: blob is persisted on local node; replication is in-progress.
10.2 Durability Guarantee
For replication_factor = N: survives loss of N-1 nodes.
10.3 Read Consistency
- local read: immediate
- remote read: after first replica arrives
- worst-case: next heartbeat round
- eventual consistency guaranteed by RedEX
10.4 Partition Semantics
Both sides may write; manifests remain causal because of hashing;
conflict resolution = content-addressing; no merges needed.

11. Operational Surface
11.1 Prometheus Metrics (per adapter + per node)
- blobs_stored_total
- blobs_fetched_total
- bytes_stored_total
- bytes_replicated_total
- blob_replication_lag_ms
- blob_gc_swept_total
- blob_gc_pending_total
- blob_disk_used_bytes
- blob_disk_capacity_bytes
11.2 CLI
net blob ls / stat <ref> / replicas <ref> / gc --dry-run / delete <ref> / pin <ref> / unpin <ref>
11.3 Health Gates
If node disk > 95%: refuse new blob replicas; advertise as blob-storage-unhealthy.

12. Future Extensions (v0.2+)
- Reed-Solomon encoding
- multi-class blob tiers (hot/cold/archive)
- trie-based manifest compression
- delta-chunking for large versioned models
- in-cluster caching layers (L1 blob cache)
```

### Appendix A.2 — capability extension (Kyra follow-up)

Reproduced verbatim. Plan crosswalk: this content informs § 7 (capability extension), § G-1 / G-2 / G-4 (gating rules), and § T-3 (mixed-capability DST scenarios).

```text
Why you *do* need capability-announcement

Blob storage is not "just another feature." It consumes:
- disk
- IO bandwidth
- replication budget
- placement slots
- gravity migration paths
- eviction space
- node health signaling

And you cannot assume every node:
- has disk
- has enough disk
- is allowed to store blobs
- wants to store blobs
- is in the right failure domain
- can handle blob replicas
- can participate in migration
- is not running compute-only workloads

This is exactly what capability tags are for.

One new capability:  capability: "blob-storage"
Three derived qualities:
  blob.disk_total_gb
  blob.disk_free_gb
  blob.replication_factor_supported = N

Dataforts uses PlacementFilter:
- P1 greedy only pulls blobs to nodes with blob-storage
- P4 gravity only migrates blobs between nodes with blob-storage
- Placement scoring can skip nodes without this capability

Why you *do NOT* need full-blown capability-driven encodings:
- no dedicated blob-storage subprotocol
- no per-node storage classes
- no advanced runtime negotiation
- no multi-capability mesh announcements
- no dynamic feature negotiation protocol
- no capability handshakes
- no versioned blob-support states

Because:
- every node already has RedEX
- every node already can replicate chunks
- every node already can publish
- every node already participates in placement

There's nothing new to negotiate at the wire level.

So what capabilities do you actually add?
1. cap.blob.storage = true|false
2. cap.blob.disk_total_gb: u64
3. cap.blob.disk_free_gb: u64                 (updated on heartbeat)
4. (optional) cap.blob.class                  (cold/warm/hot — not required for v0.1)

How Dataforts uses the capability:
- P1 greedy placement: only pulls blobs to nodes with blob.storage = true
- P4 gravity drift: moves hotter blobs to nodes with more free space;
                    evicts/migrates cold blobs from overloaded nodes
- P7 placement scoring: PlacementFilter gets a new Artifact::Blob type:
      placement_score(&Artifact::Blob, node)
  which incorporates disk_free_gb / failure-domain tags / blob-storage capability.

GREEDY IS NOT A FEATURE FLAG. IT IS A BEHAVIORAL CAPABILITY OF A NODE.

A node either:
- has spare disk / CPU / bandwidth → can act as a greedy puller
- or it doesn't → should not pull aggressively

This is a per-node trait. Greedy belongs in the capability graph, not in
global config.

Greedy influences:
- pull pressure
- bandwidth budget
- disk pressure
- migration balance
- initial replica placement
- pull scheduling

Two nodes in the same mesh might be:
- compute-heavy nodes (no greedy)
- storage-heavy nodes (greedy=true)
- hybrid nodes (greedy weight = medium)

Greedy is a local trait that Opus may surface globally, but the engine
must treat it per-node.

Correct form:
  capabilities = {
      "blob-storage": true,
      "dataforts-greedy": true|false,
      "dataforts-gravity": true|false,
  }

Later expand to:
  "dataforts-greedy-weight": u8
  "dataforts-gravity-weight": u8

Engine behavior:
  P1 Greedy (on artifact creation):
    if node.cap["dataforts-greedy"]:
        greedy_pull()
    else:
        skip_greedy

  P4 Gravity (cluster-wide), but respects:
    if !node.cap["blob-storage"]:
        do not migrate blobs here

Why greedy must be a capability instead of a flag:
Future clusters will need mixed roles:
- cold-tier storage nodes
- hot-tier greedy nodes
- compute-only nodes
- archival storage nodes
- limited-disk nodes

Global flags cannot express this. Capability tags can.

Greedy needs "scope":
- same-node / same-zone / same-region / same-cloud / whole-mesh
Because not every cluster wants greedy pulls across WAN:
- local-region clusters → greedy across full mesh is fine
- multi-region → greedy must stay local
- multi-cloud → greedy must be scoped to avoid egress cost
- edge deployments → greedy only within the same PoP

  cap.dataforts.greedy_scope = "node" | "zone" | "region" | "mesh"

Greedy needs "proximity":
- high proximity: pull aggressively from near peers only
- low proximity:  reach far if needed

  cap.dataforts.greedy_proximity: u8 (0–255)

Becomes a weight inside P1:
- high proximity = prefer closer replicas
- low proximity = allow farther replicas
- proximity = 0 = greedy disabled

Gravity needs the same fields:
  cap.dataforts.gravity_scope
  cap.dataforts.gravity_proximity

Which control:
- where gravity is allowed to migrate
- how "far" a blob/chain can drift
- which nodes compete for hot objects
- how heat propagates across topology boundaries

Minimal capability set:
  Greedy:
    cap.dataforts.greedy = true/false
    cap.dataforts.greedy_scope = enum
    cap.dataforts.greedy_proximity = u8
  Gravity:
    cap.dataforts.gravity = true/false
    cap.dataforts.gravity_scope = enum
    cap.dataforts.gravity_proximity = u8
  Blob storage:
    cap.blob_storage = true/false
    cap.blob.disk_total_gb = u64
    cap.blob.disk_free_gb = u64

Why both scope and proximity?
Different control planes:
- Scope = hard boundary  ("Do not drift into other regions.")
- Proximity = soft preference ("Prefer closer nodes even inside the region.")

Both are needed for P1 greedy / P4 gravity / P7 placement filter / mixed
node roles / multi-region meshes / multi-cloud setups / edge topologies
/ disk-pressure-aware placement.

Without them:
- greedy pulls too far
- gravity drifts across cost domains
- artifacts oscillate between zones
- blob replicas migrate out of locality
- compute locality is broken
- operators lose control over placement
```
