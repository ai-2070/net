# Dataforts Blob Storage — implementation plan (v0.3, terabyte-scale)

> Companion to [`DATAFORTS_BLOB_STORAGE_PLAN.md`](./DATAFORTS_BLOB_STORAGE_PLAN.md) (v0.2, shipped in v0.15). This is the **terabyte-class follow-up**: the v0.2 plan deliberately capped `BLOB_REF_MAX_SIZE` at 16 GiB and shipped a flat-manifest model. Workloads that need to address blobs larger than 16 GiB — ML model checkpoints, full-corpus snapshots, large media archives, raw scientific datasets — hit four load-bearing limits the v0.2 plan documented as deferred. This plan delivers all four lifts.

## Status

**Target: v0.3 — codename TBD.** Hard prerequisites:

- **v0.2** (`BlobRef::Manifest`, `MeshBlobAdapter`, refcount-driven GC, `BlobHeatRegistry`, `BlobAdapterRegistry`, capability ACL on store/delete/pin) — landed v0.15.
- **v0.2 audit-fix sweep** (bug-audit 2026-05-18: `D-*` series — short-chunk variant, sweep TOCTOU close, in-flight emission tracking, manifest chunk-size validator, `BlobError::ShortChunk`, 32-bit `MAX_STREAM_BYTES` clamp) — landed in `bugfixes-15`.
- **R-* replication hardening** (catchup backoff freshness gate, response-binding tokens, dual-leader convergence) — landed in `bugfixes-15`.

No-blocker dependencies on a major substrate change. v0.3 composes against the existing `SUBPROTOCOL_REDEX` runtime, the same `BlobAdapter` trait, and the same refcount sweep loop. The wire delta is one new `BlobRef` variant (version byte `0x03`).

## Frame

v0.2 made a deliberate trade: pick a fixed 4 MiB chunk size, encode a flat `Vec<ChunkRef>` in the manifest body, store the manifest body as itself a `BlobRef::Small` (capped at 16 GiB), and call the substrate-owned CAS shipped. That trade was correct for the v0.15 ship window — every workload the v0.15 customer set actually published fit cleanly below the 16 GiB ceiling, and the simpler addressing kept the audit surface narrow.

Four limits the v0.2 plan called out as "v0.3 work" now block the next workload class:

1. **16 GiB hard wire cap.** `BLOB_REF_MAX_SIZE = 16 * 1024^3` is encoded into every decoder (`blob_ref.rs:96`) and into `store_stream`'s memory cap (`adapter.rs:169`). A 1 TiB ML checkpoint or a 10 TiB seismic dataset cannot be addressed at all.
2. **Flat manifest body.** At 4 MiB chunks, 1 TiB = 262 144 chunk entries × ~50 bytes ≈ 13 MiB manifest body. v0.2 stores that as a `BlobRef::Small`, which technically still fits (< 16 GiB) but: a 13 MiB round trip per range-fetch is the practical bottleneck; a `Vec<ChunkRef>` of that length is a 13 MiB heap allocation on every fetch; and the body itself caps at the same 16 GiB ceiling (~310 TiB worth of chunks before structural break, but the read-cost wall hits much earlier).
3. **No content-defined chunking (CDC).** Fixed-boundary chunking means a 1-byte edit at the start of a 16 GiB file shifts every chunk boundary and produces 4096 new content hashes — zero dedup across versions. For workflows that re-upload checkpoints with small deltas (continual-learning, incremental fine-tunes), the v0.2 store burns bandwidth proportional to the entire file on every revision.
4. **Replication overhead at full-copy scale.** A 1 TiB blob replicated at `replication_factor = 3` costs 3 TiB of disk across the placed set. Reed–Solomon (10+4) brings the same durability properties down to 1.4 TiB. v0.2 reserved the `Encoding::ReedSolomon { k, m }` enum slot on the wire but shipped the implementation as `Replicated` only.

The four lifts are mostly orthogonal: (1) and (2) co-design as a hierarchical manifest tree; (3) is a chunking-strategy axis selectable per channel; (4) is an encoding axis selectable per blob. v0.3 ships all four behind feature gates so operators can adopt incrementally.

## What ships

Six things, in dependency order:

1. **`BlobRef::Tree` wire variant** — a hierarchical manifest whose internal nodes carry `(child_hash, child_total_size)` pairs and whose leaves carry `ChunkRef` lists. Lifts the effective addressable size from 16 GiB to ~16 EiB (limited by `u64` total_size). Wire version `0x03`; v0.2's `Small` (`0x01`) and `Manifest` (`0x02`) are unchanged.
2. **Bounded-memory `store_stream`** — `MeshBlobAdapter::store_stream` rewritten to consume the input stream chunk-by-chunk, persist each chunk synchronously, and accrete the manifest tree incrementally. Memory footprint becomes O(chunk_size × in_flight_chunks + fanout × tree_depth) — bounded regardless of total blob size.
3. **Streaming + range fetch over `BlobRef::Tree`** — `fetch` and `fetch_range` walk the tree on demand, fetching only the manifest path + the spanning chunks. `O(depth + range / chunk_size)` chunk fetches per range query.
4. **Content-defined chunking (CDC) strategy** — `ChunkingStrategy::Cdc { avg, min, max }` selectable per channel via `RedexFileConfig`. Default stays `Fixed { size: 4 MiB }` for back-compat. Variable-size chunks survive in the v0.3 `ChunkRef` (already a `(hash, size)` pair).
5. **Reed–Solomon erasure coding (10+4 default)** — selectable per blob via `Encoding::ReedSolomon { k, m }`. Each stripe-group of `k` data chunks gets `m` parity chunks computed at store time; fetch tolerates up to `m` missing per stripe by reconstruction.
6. **Resume + per-stream bandwidth class** — partial-transfer recovery keyed on per-chunk hash (receiver already has chunks [0..N); only [N..M) flow on resume). Producers tag streams with `BandwidthClass::{Background, Foreground, Realtime}` so a TB-scale background backfill doesn't starve interactive workloads on the same replication budget.

What this plan does NOT ship (explicitly deferred):

- **Cross-mesh blob replication.** A subnet-A blob referenced from subnet-B still resolves through the subnet gateway's existing relay path; no native cross-subnet replication.
- **Live edit + delta storage.** A `BlobRef::Tree` is immutable; an edited blob is a new `BlobRef::Tree` whose tree shares chunks with the original via content addressing (good for CDC) but is structurally a fresh root. No mutable-blob abstraction.
- **Compression on the chunk path.** Chunks store as-is. Operators wanting compression run their producer-side compression (gzip, zstd, codec-specific) before `store_stream`; the v0.3 layer does not compose codec dictionaries.
- **Per-replica geographic placement constraints for blob stripes.** Reed-Solomon stripes inherit the channel's existing `PlacementFilter`; the v0.3 implementation does not add a separate "spread parity chunks across racks" constraint axis. Operators with rack-aware placement needs configure it via the existing `PlacementFilter` axes; tighter blob-stripe-specific constraints land in a follow-up if a workload demands them.
- **Manifest compaction across versions.** Two `BlobRef::Tree` revisions of the same logical file that differ in one chunk share chunks via dedup but each carry their own full manifest tree. No shared-prefix manifest layout.
- **Streaming-compute-on-blob.** Operators wanting to fold a TB blob through a daemon read it via `fetch_range` chunk-by-chunk; no native "stream a blob to a daemon" surface in v0.3.

---

## Design

### 1. `BlobRef::Tree` wire variant

```rust
pub enum BlobRef {
    /// v0.15 — inline single-hash. Wire version `0x01`. UNCHANGED.
    Small { hash: [u8; 32], size: u32 },

    /// v0.2 — flat manifest, capped at 16 GiB. Wire version `0x02`.
    /// UNCHANGED. Still emitted by producers below the
    /// `TREE_THRESHOLD_BYTES` boundary (default 256 GiB) for round-trip
    /// efficiency on small-but-chunked blobs.
    Manifest { encoding: Encoding, chunks: Vec<ChunkRef>, size: u64 },

    /// v0.3 — hierarchical manifest. Wire version `0x03`.
    /// `root_hash` references a `TreeNode` blob (stored as `Small`)
    /// that contains either child `TreeNode` hashes or a leaf
    /// `Vec<ChunkRef>`. `depth` is informational — pinned in the
    /// outer ref so a verifier can sanity-check the actual depth
    /// without descending. `total_size` is the byte length of the
    /// reconstructed blob; the substrate trusts it under the same
    /// model `BlobRef::Manifest::size` already does, with a
    /// cross-check at decode (sum of leaf chunk sizes == total_size).
    Tree {
        encoding: Encoding,
        root_hash: [u8; 32],
        total_size: u64,
        depth: u8,
    },
}
```

`TreeNode` is a separately-addressable blob:

```rust
pub enum TreeNode {
    /// Internal node — carries up to `FANOUT` children, each with
    /// the subtree's total byte size for O(depth) prefix-sum
    /// lookup. Children may be either `TreeNode` blobs or, at the
    /// last level, `LeafNode` blobs (the discriminant is implicit:
    /// `depth` on the outer `BlobRef::Tree` says where the
    /// leaves are; nothing in the wire node identifies its own
    /// position because that would let a peer-supplied node lie).
    Internal { children: Vec<(/* hash */ [u8; 32], /* subtree_size */ u64)> },

    /// Leaf node — same shape as v0.2 `ManifestBody::chunks`,
    /// reused verbatim so leaves are wire-compatible with v0.2
    /// `Manifest` bodies. A v0.2 reader cannot reach a `Tree`'s
    /// leaves (it doesn't know to descend), but the leaf encoding
    /// is identical so the test fixtures port over cleanly.
    Leaf { chunks: Vec<ChunkRef> },
}
```

`FANOUT = 256` (a tunable but pinned in v0.3). At 4 MiB chunks and `FANOUT = 256`, a leaf addresses 1 GiB; an internal node at depth 1 addresses 256 GiB; depth 2 → 64 TiB; depth 3 → 16 PiB. The `depth` field on `BlobRef::Tree` saturates at 4 in v0.3 to bound the per-fetch manifest path; deeper trees route through `Tree` chaining (a `Tree` whose root references another `Tree` blob), and the v0.3 codepath rejects depth > 4 with a typed error.

Wire encoding follows the existing pattern: `[4-byte magic][1-byte version=0x03][postcard-encoded body]`. The body is `(encoding, root_hash, total_size, depth)` — fixed 1 + 1 + 32 + 8 + 1 = 43 bytes regardless of blob size. `TreeNode` is itself wrapped as a `BlobRef::Small` and stored at `dataforts/blob/<hex32>` — same channel naming as v0.2 chunks, same refcount lifecycle.

Producers SHOULD emit `Tree` above `TREE_THRESHOLD_BYTES = 256 GiB` (one full leaf at fanout 256 × chunk 1 GiB; 256 chunk entries fit comfortably in a leaf, and below the threshold the flat-manifest path stays cheaper). Below threshold, emit `Manifest` for round-trip efficiency. The threshold is an operator hint, not a wire requirement: a `Tree` of size 1 KiB is well-formed and a `Manifest` of size 16 GiB is well-formed; the threshold sets producer policy only.

### 2. Bounded-memory `store_stream`

```rust
impl MeshBlobAdapter {
    pub async fn store_stream(
        &self,
        blob_ref: &BlobRef,        // contains expected hash + total_size
        stream: BlobByteStream,
        size_hint: Option<u64>,
        chunking: ChunkingStrategy,
    ) -> Result<(), BlobError> { ... }
}
```

The implementation pipeline:

1. **Chunker** — drains the stream into a content-defined or fixed-size chunker. Emits `(bytes, hash)` pairs as chunk boundaries are reached. Bounded buffer: `MAX_CHUNK_SIZE_BYTES = 64 MiB` (one chunker working buffer) for CDC; 4 MiB for Fixed.
2. **Chunk store fan-out** — emitted chunks dispatch to `MeshBlobAdapter::store_chunk` with bounded concurrency (`STORE_PARALLELISM = 16`). Each `store_chunk` opens / appends to the chunk's `RedexFile` (same path as v0.2). Backpressure: the chunker awaits when 16 stores are in flight.
3. **Manifest tree builder** — a stack of `Vec<ChunkRef>` (leaf) and `Vec<(hash, subtree_size)>` (internal) builders. When the leaf builder reaches `FANOUT` entries, close the leaf: serialize as a `TreeNode::Leaf`, store as a `BlobRef::Small`, push `(leaf_hash, leaf_size)` onto the depth-1 builder. Cascade closure up the tree as each level fills.
4. **End-of-stream** — close every open builder level, all the way to the root. Emit the final `BlobRef::Tree` with the root hash and total size.

Memory bound: O(`chunk_buffer` + `STORE_PARALLELISM × chunk_size` + `FANOUT × tree_depth × ChunkRef_size`). At defaults: 64 MiB + 16 × 64 MiB + 256 × 4 × ~50 bytes ≈ 1.1 GiB worst-case. For typical Fixed-chunking workloads: 4 MiB + 16 × 4 MiB + 51 KiB ≈ 68 MiB. **Bounded regardless of input size.**

A pre-fix `store_stream` that buffered the whole blob first cannot be retained as a default fallback for adapters that don't override — the default trait impl's 16 GiB cap is the right behaviour for adapters that genuinely accumulate a buffer. `MeshBlobAdapter::store_stream` overrides explicitly. The v0.3 trait surface adds a `chunking: ChunkingStrategy` parameter with a default that preserves the v0.2 behaviour for adapters that don't care.

### 3. Streaming + range fetch over `BlobRef::Tree`

```rust
impl MeshBlobAdapter {
    pub async fn fetch_range(
        &self,
        blob_ref: &BlobRef,
        range: Range<u64>,
    ) -> Result<Vec<u8>, BlobError> { ... }
}
```

For `BlobRef::Tree`:

1. Fetch the root `TreeNode` (the blob at `root_hash`).
2. If it's `Internal`, locate the child whose cumulative byte range covers `range.start` via prefix-sum scan. Recurse into that child.
3. If `range` spans multiple children, fan out one descent per spanning child concurrently (bounded `FETCH_PARALLELISM = 8`).
4. At the leaf, walk the `Vec<ChunkRef>` to find the spanning chunks (fixed-size: direct index; CDC: prefix-sum scan).
5. Fetch the spanning chunks (already-existing `fetch_chunk` path); slice the returned bytes to `range`.

For `BlobRef::Manifest` (v0.2): use the existing `byte_range_to_chunks` helper unchanged.

For `BlobRef::Small`: existing path unchanged.

**Manifest-fetch caching.** A per-node LRU on `[u8; 32] → TreeNode` (default 64 MiB / ~16 K leaves at fanout 256) absorbs adjacent range reads on the same blob without re-fetching the manifest path. The cache participates in the existing `BlobHeatRegistry` so hot trees pin in cache via the same data-gravity mechanism that pins hot chunks.

**Tree-walk verification.** Each fetched `TreeNode` is verified against the hash the parent (or `BlobRef::Tree::root_hash` for the root) advertised. A peer-supplied node whose body doesn't hash to the expected value surfaces as `BlobError::HashMismatch` and aborts the descent — the v0.2 single-chunk verification model extends naturally up the tree.

### 4. Streaming verification

The v0.2 model verifies each chunk's BLAKE3 against the manifest's stored hash on `fetch`. v0.3 layers the same property across the manifest tree:

- Root: `blake3(root_TreeNode_bytes) == BlobRef::Tree::root_hash`.
- Internal node at depth `d`: `blake3(node_bytes) == parent.children[i].0`.
- Leaf node: same as internal.
- Chunk: `blake3(chunk_bytes) == leaf.chunks[i].hash` (v0.2 behaviour unchanged).

Property: **any prefix of the blob can be verified before the suffix arrives.** A consumer streaming a 1 TiB blob can hand verified bytes to its caller as each chunk arrives; a tampered chunk fails its hash check and the stream aborts at the point of corruption, not after a 1 TiB read.

Property: **no Merkle proof needed for partial fetches.** The tree-walk verification trivially proves chunk authenticity against the root hash by the chain of node hashes traversed. A future "blob proof" surface (compact `(chunk, path-of-hashes)` for a third-party verifier) composes against this design but is not v0.3 scope.

### 5. Content-defined chunking (CDC)

Selectable per channel via `RedexFileConfig`:

```rust
pub enum ChunkingStrategy {
    /// v0.2 behaviour — fixed-size chunks. Default for back-compat.
    Fixed { size: u32 },
    /// FastCDC-style content-defined chunking. Boundaries fall at
    /// content-dependent positions, so a small edit only invalidates
    /// the chunk containing the edit (plus possibly one neighbor
    /// across the new boundary), not the entire suffix.
    Cdc { avg: u32, min: u32, max: u32 },
}

impl Default for ChunkingStrategy {
    fn default() -> Self { Self::Fixed { size: 4 * 1024 * 1024 } }
}
```

**FastCDC** (Xia et al., 2016) is chosen over Rabin chunking for: deterministic output (same content → same boundaries across implementations and across language bindings), bounded compute (~150 MB/s/core single-pass), and library availability (`fastcdc` crate, MIT). Defaults: `avg = 4 MiB`, `min = 1 MiB`, `max = 16 MiB`. Avg matches v0.2's fixed size so cross-strategy dedup is plausible for content that happens to land on a CDC boundary near 4 MiB; min/max bound the variance so prefix-sum lookups stay cheap and a pathological input can't produce thousand-chunk explosion.

CDC chunks store via the same `dataforts/blob/<hex32>` path. The `ChunkRef::size` field (already a `u32`) carries the actual chunk size — variable per chunk under CDC, constant under Fixed. Leaf nodes carrying CDC chunks use the same wire shape as fixed-chunk leaves; the reader differentiates only at range-fetch time (prefix-sum scan vs direct index).

**Per-channel selection.** The channel's `RedexFileConfig::chunking` (new field) pins the producer's strategy at store time. Consumers don't care about the producer's strategy at all — they read `ChunkRef::size` from the leaf and slice accordingly. A blob stored with CDC and a blob stored with Fixed can coexist on the same channel; the strategy is only relevant at chunking time.

**Migration of v0.2 blobs.** A v0.2 `BlobRef::Manifest` is implicitly Fixed-chunked (all chunks at 4 MiB, except the last). v0.3 readers handle both `Manifest` and `Tree` paths; producers should not re-chunk existing v0.2 blobs (that would invalidate every consumer's chunk-hash cache).

### 6. Reed–Solomon erasure coding

The v0.2 `Encoding` enum already reserves `ReedSolomon { k, m }`. v0.3 implements it:

**Store path.** When a producer publishes with `Encoding::ReedSolomon { k, m }`:
1. Chunker emits chunks normally.
2. Every `k` consecutive chunks form a **stripe group**. The group's `m` parity chunks are computed via systematic Reed-Solomon over Galois field `GF(2^8)` at chunk-byte granularity. Parity chunks have their own BLAKE3 hashes and store at `dataforts/blob/<hex32>` like any other chunk.
3. The leaf `TreeNode` lists all `k + m` chunks of the stripe in order (data chunks then parity chunks) with each `ChunkRef` carrying a new `role: ChunkRole` field.

```rust
pub enum ChunkRole {
    Data,
    Parity { stripe_index: u8 },
}

pub struct ChunkRef {
    pub hash: [u8; 32],
    pub size: u32,
    pub role: ChunkRole,    // new in v0.3
}
```

Defaults: `k = 10, m = 4` (1.4× storage overhead, tolerates any 4 chunk losses per stripe). Other configurations: `(6, 3)` for 1.5× / 3-loss; `(20, 4)` for 1.2× / 4-loss; `(3, 2)` for 1.67× / 2-loss in small-cluster deployments. The wire `k, m: u8` allows up to `(255, 255)`; v0.3 rejects `k + m > 255` at the producer-side validator.

**Fetch path.**
1. Resolve the leaf for the range. Identify the stripe.
2. Optimistic: fetch the `k` data chunks of the stripe in parallel. If all succeed, slice and return.
3. Reconstruction: on any data-chunk fetch failure (`NotFound`, `HashMismatch`, `ShortChunk`, `Cancelled`), fetch enough parity chunks to make the total available count ≥ `k`. Reconstruct the missing data chunk(s) via the inverse RS matrix.
4. Failure: if more than `m` chunks of the stripe are missing (combined data + parity), return `BlobError::Backend("erasure: stripe N unrecoverable, {} chunks lost")`.

Reconstruction reads the missing-chunk slice into a fresh buffer and returns it to the caller, but does NOT auto-restore the missing data chunk on disk by default. A separate **repair sweep** (operator-triggerable; opt-in scheduled at `RedexFileConfig::blob_repair_cadence`) walks reachable stripes, detects degraded ones (< `k` data chunks present), and stores reconstructed data chunks back to their content-addressed path. Without auto-restore, every fetch on a degraded stripe re-computes the reconstruction — fine for cold blobs, not great for hot ones. The repair sweep closes the loop.

**Per-blob selection.** The blob's `BlobRef::Tree::encoding` pins the encoding for every chunk in that blob. A `Replicated` blob and a `ReedSolomon` blob coexist on the same channel — encoding is a property of the BlobRef, not the channel.

**RS library.** `reed-solomon-erasure` crate (MIT, pure Rust, SIMD-accelerated on x86-64 / aarch64). Performance: ~3 GB/s for `(10, 4)` encoding on a single core; reconstruction is similar throughput. The conformance suite pins encode/decode against fixture vectors so cross-version compatibility holds across substrate versions.

### 7. Resume + per-stream bandwidth class

**Resume.** A stream that fails partway through is fundamentally addressable by `BlobRef`. The receiver's local refcount table already records every chunk it has fetched; a `fetch_range` retry asks the substrate for chunks the receiver doesn't have. No explicit resume token: the BlobRef IS the resume token. The substrate's fetch path queries `BlobRefcountTable::contains` per chunk; chunks already present are skipped.

This is correct by accident in v0.2 too — but at TB scale the "10 hours into a 12 hour download" failure mode becomes the common case, so v0.3 ships an explicit operator-facing **resume metrics** surface: `blob_fetch_resumed_total`, `blob_fetch_chunks_skipped_on_resume_total`, `blob_fetch_bytes_skipped_on_resume_total`. Operators see the resume effectiveness on the dashboard without instrumenting per-fetch.

**Bandwidth class.** Per-stream priority lets a 1 TiB background backfill share the replication budget with interactive 10 MiB chunk fetches without starving them. New `BandwidthClass`:

```rust
pub enum BandwidthClass {
    /// Long-running TB-scale background work. Bounded to the
    /// configured `replication_budget.background_fraction` of the
    /// per-channel rate. Default: 30 %.
    Background,
    /// Default. Interactive fetches, normal user workload.
    Foreground,
    /// Operator-pinned. Bypasses budget if disk pressure allows.
    /// Reserved for control-plane traffic (config publish, ACL
    /// updates) and operator-triggered repair sweeps.
    Realtime,
}
```

Each `store_stream` / `fetch` / `fetch_range` accepts a `BandwidthClass` (defaulting to `Foreground` for source-compat). The replication budget consults the class to gate `try_consume`: a `Background` stream is admitted only when the budget has at least `(1 - background_fraction) × capacity` available; a `Foreground` stream is admitted normally; a `Realtime` stream bypasses the rate limit but still respects disk-pressure circuit-breakers.

Per-stream queues at the dispatcher level keep classes from inter-blocking: `Foreground` always polls before `Background` in the per-channel send loop. A `Background` stream that's been starved for > 60 s gets promoted to `Foreground` temporarily so a wedged backfill recovers without operator intervention (anti-starvation hatch).

### 8. Operator surface

**Metrics** (Prometheus):
- `blob_tree_depth_avg` — average depth of trees stored locally.
- `blob_tree_node_cache_hit_ratio` — manifest-cache effectiveness.
- `blob_fetch_resumed_total` — fetches that observed at least one pre-fetched chunk.
- `blob_fetch_chunks_skipped_on_resume_total` — chunks the receiver already had.
- `blob_erasure_reconstructions_total` — fetch paths that hit the parity branch.
- `blob_repair_chunks_restored_total` — chunks restored by the repair sweep.
- `blob_cdc_chunks_total` / `blob_fixed_chunks_total` — per-strategy chunk count.
- `blob_dedup_chunks_avoided_total` — chunks not re-stored because the hash already existed (already counted by v0.2 via the idempotent-store path; surfaced as a separate gauge in v0.3).

**CLI extensions** to `net blob`:
- `net blob tree <ref>` — show the tree structure (root + depth + per-level summary).
- `net blob path <ref> --byte <N>` — show the chunks containing byte `N` (path through the tree).
- `net blob repair <ref> [--dry-run]` — walk reachable stripes, identify degraded ones, restore missing data chunks. Idempotent.
- `net blob verify <ref>` — fetch and verify the entire tree end-to-end. Useful after a suspected silent corruption event.
- `net blob throughput <ref>` — report per-stream throughput and bandwidth class for an in-flight fetch.

**Capability tags**:
- `dataforts:blob-tree-supported` — node advertises v0.3 tree support; v0.2-only nodes don't advertise this tag, and producers that need cross-cluster v0.2 compatibility downgrade to `BlobRef::Manifest` when targeting a peer without the tag.
- `dataforts:blob-erasure-supported` — node advertises RS encode/decode capability.
- `dataforts:blob-cdc-supported` — node advertises CDC chunking capability.

The three tags are independent — a v0.3 node may support trees but not erasure, etc. The `MeshBlobAdapter` consults the destination's tag set before publishing a `Tree` or RS blob; mismatches downgrade to the simpler shape the destination understands.

### 9. Migration path / wire compat

**Forward compat: v0.2 → v0.3.** v0.2 readers don't know how to decode `BlobRef::Tree` (version `0x03`); they return `BlobError::UnsupportedVersion(3)`. Producers on v0.3 emit `Tree` only above `TREE_THRESHOLD_BYTES` AND only when the receiver advertises `dataforts:blob-tree-supported`. Below threshold, or to a v0.2-only receiver, v0.3 emits the existing `Manifest` (capped at 16 GiB) — the v0.2 receiver decodes it unchanged.

**Back compat: v0.3 reads v0.2.** v0.3 readers handle `Small` / `Manifest` / `Tree` exhaustively. Existing v0.2 blobs flow through the v0.3 reader's `Manifest` branch unmodified. The v0.3 reader's `Tree` branch is gated on the version byte; no risk of mis-decoding a `Manifest` as a `Tree`.

**No re-chunking required.** A v0.2 `Manifest` is implicitly Fixed-chunked at 4 MiB. A v0.3 producer can leave the existing blob untouched (consumers continue to fetch via the `Manifest` path) OR re-store the blob under v0.3 with `Encoding::ReedSolomon` for the storage savings. Re-storing is a separate operator action and creates a new BlobRef; the old one is independently refcounted and GCs on its own cycle.

**Erasure-coded blobs cannot downgrade.** A `ReedSolomon`-encoded `Tree` cannot be read by a v0.2 node OR a v0.3 node without erasure support. The producer-side downgrade is `Replicated` ↔ `ReedSolomon` only — there is no graceful "fall back to replication for a v0.2 receiver" path because the chunks themselves carry parity (the parity chunks would be unintelligible to a v0.2 reader looking for data chunks). Operators publishing to mixed-version clusters use `Replicated` until v0.3 is uniform.

### 10. Performance + correctness targets

| Surface | v0.2 | v0.3 target |
|---|---|---|
| Max addressable blob size | 16 GiB | 16 EiB (= u64 ceiling) |
| `store_stream` peak memory | ~size of blob | ~68 MiB (Fixed) / ~1.1 GiB (CDC) |
| Range-fetch latency at 1 TiB scale | ~13 MiB manifest + 1 chunk fetch | ~32 KiB × depth manifest + 1 chunk fetch |
| Manifest-path bytes for 1 TiB | ~13 MiB | ~16 KiB (depth 2: root 8 KiB + leaf 8 KiB) |
| Storage cost at `replication_factor=3` | 3× | 1.4× (RS 10+4) |
| Cross-revision dedup (small edits) | 0 % (Fixed) | ~99 % (CDC, single-chunk delta) |
| Resume effectiveness on 50% partial transfer | 0 % (no resume) | ~50 % (chunks already on disk are skipped) |
| TB-scale `store_stream` throughput | N/A (cap) | ≥ 200 MB/s on a 1 Gbps link |
| Manifest cache memory footprint | N/A | ≤ 64 MiB default, configurable |

**Correctness invariants** (pinned by conformance suite):
- A blob stored as `Tree` with `Replicated` encoding round-trips byte-identically under any combination of chunk-loss-then-fetch (where chunks-lost ≤ `replication_factor - 1`).
- A blob stored as `Tree` with `ReedSolomon { k, m }` encoding round-trips byte-identically under any combination of chunk-loss-then-fetch (where chunks-lost-per-stripe ≤ `m`).
- A blob stored via CDC chunking round-trips byte-identically (CDC boundaries are deterministic from content).
- A blob stored once and fetched repeatedly returns identical bytes (idempotent content addressing).
- Two producers chunking the same content with the same chunking strategy produce identical `BlobRef::Tree` roots (chunk dedup at storage level).
- Tree-walk verification rejects any peer-supplied node whose hash doesn't match the parent's stored hash.

---

## Phase breakdown

Phase A → D below are designed for incremental ship — each phase delivers operator-visible value and ships behind a feature gate so v0.2 wire compatibility never breaks.

### Phase A — Hierarchical manifests (BlobRef::Tree)

**Goal:** lift the 16 GiB cap. Ship `Tree` wire variant + streaming store/fetch + cache.

**Deliverables:**
- `BlobRef::Tree` enum variant, postcard encoding, decoder + chunk-size cross-check.
- `TreeNode` enum + per-node BLAKE3 verification at fetch.
- `MeshBlobAdapter::store_stream` rewritten to chunk-and-tree-build with bounded memory.
- `MeshBlobAdapter::fetch_range` walks the tree on demand.
- LRU manifest cache at the node level (configurable cap, default 64 MiB).
- `dataforts:blob-tree-supported` capability tag + producer-side downgrade to `Manifest` when destination doesn't advertise it.
- Cross-language bindings (Node, Python) for `BlobRef::Tree`.
- Conformance fixtures: round-trip a 1 GiB and a 100 GiB blob through Tree; verify cache hit ratio under repeated range fetches.

**Doesn't ship in A:** CDC, RS, bandwidth classes, repair sweep.

**Test plan:** existing blob conformance suite extended with `TreeRoundTrip` cases; new `tree_walks` unit tests on `MeshBlobAdapter`; new `tree_node_cache` tests; one integration test that builds + fetches a 100 GiB blob with `Tree` and asserts disk usage matches `replication_factor × total_size` (no manifest body explosion).

**Effort estimate:** 2-3 weeks. Largest risk: getting the tree-builder boundary semantics exactly right under streaming fan-out. Mitigated by reusing the v0.2 chunker (already correct) and treating tree-build as a separate state machine on top.

### Phase B — Content-defined chunking

**Goal:** dedup-aware chunking for revised blobs.

**Deliverables:**
- `ChunkingStrategy` enum + per-channel `RedexFileConfig::chunking` field.
- FastCDC chunker (`fastcdc` crate integration).
- `dataforts:blob-cdc-supported` capability tag.
- Bindings expose `ChunkingStrategy::Cdc { ... }` so SDK callers can opt in.
- Conformance fixtures: store a 1 GiB blob with CDC; edit one byte at offset 500 MiB; re-store; assert ≥ 99% of chunks are deduped.

**Doesn't ship in B:** anything else (RS, bandwidth classes, repair).

**Test plan:** CDC determinism (same content → same boundaries) across language bindings; CDC + Tree round-trip; CDC + range fetch; CDC variance test (boundary distribution within `[min, max]`).

**Effort estimate:** 1-2 weeks. Largest risk: cross-language CDC determinism. Mitigated by pinning the FastCDC variant + parameters in a shared spec doc and testing the Node/Python bindings against the same fixture vectors.

### Phase C — Reed–Solomon erasure coding

**Goal:** storage cost reduction at the same durability.

**Deliverables:**
- `ChunkRef::role` field + `ChunkRole` enum.
- `Encoding::ReedSolomon { k, m }` implementation (encode at store, decode at fetch).
- Repair sweep (`net blob repair`).
- `dataforts:blob-erasure-supported` capability tag.
- Conformance fixtures: store a 100 GiB blob with `(10, 4)`; kill 3 chunks per stripe; assert fetch succeeds via reconstruction; kill 5 chunks per stripe; assert fetch fails cleanly.

**Doesn't ship in C:** auto-repair (operator must run `net blob repair` explicitly).

**Test plan:** stripe-boundary correctness on the chunk-fan-out path; reconstruction byte-identical to original; repair sweep doesn't restore parity chunks (only data); concurrent fetch + repair doesn't corrupt the reconstruction.

**Effort estimate:** 2-3 weeks. Largest risk: Galois-field reconstruction edge cases. Mitigated by leaning on the well-tested `reed-solomon-erasure` crate and pinning fixture vectors from its test suite.

### Phase D — Operational: bandwidth classes + resume metrics

**Goal:** make TB-scale fetches operationally sane.

**Deliverables:**
- `BandwidthClass` enum + per-call parameter on `store_stream` / `fetch`.
- Replication-budget integration: per-class admission gating.
- Per-channel send-queue priority by class.
- Anti-starvation hatch: `Background` → `Foreground` promotion at 60s starve.
- Resume metrics (`blob_fetch_resumed_total`, etc.).
- CLI: `net blob throughput`, `net blob tree`, `net blob path`, `net blob verify`.

**Test plan:** background + foreground co-existence test (background starves to bound; foreground unaffected); resume metric correctness across a fetch interrupted at 50%; starvation hatch fires at 60s mark.

**Effort estimate:** 1-2 weeks. Largest risk: tuning the starvation thresholds. Mitigated by making them `RedexFileConfig` knobs with conservative defaults.

---

## Risks

1. **Manifest tree fan-out tuning.** Fanout 256 is a guess; the actual optimal depends on chunk count distributions in real workloads. Too-low fanout → deeper trees → more manifest fetches per range; too-high fanout → larger leaves → more bytes wasted on partial range reads. v0.3 ships fanout as a tunable in `RedexFileConfig` with default 256; a follow-up could auto-tune based on observed access patterns.

2. **CDC boundary stability across implementations.** FastCDC has multiple variants in the wild (`v2017`, `v2020`, gear table choice). v0.3 pins to a specific variant + gear table; cross-language bindings must use byte-identical implementations. Mitigated by including FastCDC fixture vectors in the conformance suite and requiring all bindings to pass them.

3. **Erasure reconstruction latency.** On a degraded stripe, `(10, 4)` reconstruction fetches 14 chunks (10 data + up to 4 parity) and runs the inverse-matrix decode. Fetch RTT × 14 + decode CPU = real latency. Mitigated by: hot blobs trigger the repair sweep so degraded stripes don't stay degraded; cold blobs are reconstructed once per fetch (acceptable at TB scale where fetches are themselves rare).

4. **Cache invalidation for hot blob trees.** A manifest cache keyed on root_hash never needs invalidation (content-addressed), but a hot blob whose tree exceeds the cache size churns the cache on every fetch. Mitigated by: cache size is configurable; large trees touch many leaves so the LRU's natural eviction handles the worst case; data-gravity pinning prevents hot trees from evicting under cold-tree pressure.

5. **Bandwidth class starvation under sustained background load.** A `Background` stream blocked by a steady stream of `Foreground` fetches never makes progress until the 60s hatch fires. For multi-TB backfills this could mean the backfill never finishes if the cluster is under sustained interactive load. Mitigated by the hatch (60s isn't unreasonable for a TB-scale job); operators can configure a tighter hatch or pin the backfill to `Foreground` for known-long jobs.

6. **Repair sweep load.** Walking every reachable `BlobRef::Tree` to detect degraded stripes is O(N chunks) per sweep. At a million chunks, that's a real chunk-stat I/O load. Mitigated by sweep cadence being operator-configurable (default 24 h), and by using the existing refcount table's metadata for the walk so no fresh disk reads are needed for the detection phase.

7. **Wire-format churn.** v0.3 adds one wire variant (`0x03`) and one new field on `ChunkRef` (`role`). The `role` field is the riskier of the two because it lands inside an already-shipped leaf encoding — a v0.2 reader sees an unknown field and either errors or silently drops it depending on postcard behaviour. Mitigated by either: (a) bumping leaf encoding to a v0.3-only shape (`TreeNode::Leaf` vs `ManifestBody::chunks` differ at the wire level), OR (b) packing `role` into a reserved high bit of `ChunkRef::size`. Plan A is cleaner; plan B saves bytes. Open question — see below.

---

## Open questions

1. **`ChunkRef::role` wire shape.** Option A: bump the leaf encoding so v0.3 `TreeNode::Leaf` carries `Vec<ChunkRefV3>` (a new struct including `role`) while v0.2 `ManifestBody::chunks` stays `Vec<ChunkRefV2>` (no `role`). Cleaner; no risk of stale postcard semantics. Option B: pack `role` into bit 31 of `ChunkRef::size` (chunks > 2 GiB are out of spec anyway). Saves wire bytes but complicates the decoder. **Recommendation: Option A.**

2. **CDC parameters.** `avg = 4 MiB, min = 1 MiB, max = 16 MiB` is the default proposed above. Workloads with mostly-small payloads (logs, audit events) might prefer `avg = 256 KiB` for finer dedup; ML checkpoints might prefer `avg = 32 MiB` for fewer chunks per blob. Should v0.3 ship presets (`ChunkingStrategy::cdc_for_workload(WorkloadHint::Logs)`) or just expose raw `avg/min/max` and document common patterns? **Recommendation: raw params + docs; presets are easy to add later.**

3. **Erasure-coded chunk channel naming.** Data chunks live at `dataforts/blob/<hex32>`. Parity chunks have their own BLAKE3 hashes (over the parity bytes) and could live at the same path. The downside is operators auditing a chunk file can't tell from the name whether it's data or parity. Should parity chunks live at `dataforts/blob-parity/<hex32>` to disambiguate? **Recommendation: keep at `dataforts/blob/<hex32>` (uniform GC, uniform replication, uniform refcounting); operators query the parent `Tree`'s leaf to learn the role.**

4. **Per-channel default encoding.** Should `RedexFileConfig` carry a default `Encoding` (so all blobs published to a channel inherit RS without per-call opt-in)? Cleaner UX, but operators may want some blobs replicated and others erasure-coded on the same channel. **Recommendation: per-call only in v0.3; per-channel default as v0.4 follow-up if a workload demands it.**

5. **Tree depth cap.** v0.3 caps at depth 4 (≈ 1 EiB at fanout 256). A workload approaching 1 EiB is hypothetical; the cap exists to bound per-fetch path length. Should the cap be configurable (`RedexFileConfig::blob_tree_max_depth`) for synthetic-workload testing? **Recommendation: cap is a constant in v0.3; revisit if a real workload approaches it.**

6. **Manifest cache shared with greedy LRU.** The proposed manifest cache is separate from the greedy LRU cache. A combined cache would unify the eviction model but complicate the cache's value type (chunks vs tree nodes). **Recommendation: separate cache in v0.3; merge in a follow-up if dual-cache memory pressure shows up in production.**

---

## Appendix — Estimated v0.2 → v0.3 LOC delta

| Surface | Net LOC |
|---|---|
| `blob_ref.rs` (Tree variant + decoder) | +400 |
| `blob_tree.rs` (new — TreeNode + builder + walker) | +800 |
| `mesh.rs` (store_stream rewrite + fetch_range tree walk) | +600 |
| `cdc.rs` (new — FastCDC chunker) | +250 |
| `erasure.rs` (new — RS encode/decode + repair) | +500 |
| `bandwidth_class.rs` (new — per-stream priority) | +200 |
| `BlobError` extensions | +100 |
| FFI + bindings | +400 |
| Tests + conformance | +1500 |
| Docs (in-tree comments) | +400 |
| **Total** | **~5150** |

For reference, v0.2 (`DATAFORTS_BLOB_STORAGE_PLAN.md`) shipped ~19 000 LOC. v0.3 is ~27% of v0.2's footprint — additive on a stable base.

---

## What we are NOT solving

To be explicit about what the v0.3 plan deliberately leaves alone, even at TB+ scale:

- **Versioned blobs / mutable references.** A blob is immutable by hash. Versioning lives at the application level (e.g. a chain whose events carry successive BlobRefs).
- **Blob ACLs distinct from channel ACLs.** A blob inherits its publishing channel's ACL via the existing `AuthGuard`. Per-blob ACL distinct from channel ACL is a separate authorization design.
- **Cross-region replication policies for blobs.** Blobs replicate per the channel's `PlacementFilter`, same as today. A blob-specific replication policy (e.g. "this 10 TB dataset replicates only to the EU racks") is operator placement-filter work, not a blob-layer feature.
- **Encryption at rest.** Chunks store as-is on disk. Disk-level encryption (LUKS, file-system encryption) is the operator's responsibility; the blob layer trusts the local disk. A blob-layer encryption design (per-blob AES-GCM with a key managed via the identity layer) is a separate roadmap item.
- **Compression on the blob path.** Operators that want compression run their producer-side compression (gzip, zstd) before `store_stream`. Native compression composes against the chunking strategy in ways that vary per workload — punted to a future plan if a workload demonstrates the need.

---

## Decision log

| Decision | Choice | Rationale |
|---|---|---|
| Wire ceiling lift mechanism | Hierarchical manifest tree | One additional wire variant; lifts cap from 16 GiB to 16 EiB. Alternative (raise `BLOB_REF_MAX_SIZE`) keeps the flat-manifest read-cost wall. |
| Tree fanout | 256 | Balances depth (3 levels covers 16 PiB) vs leaf-fetch granularity (1 GiB per leaf). |
| Max tree depth | 4 | Bounds per-fetch manifest path. ~1 EiB ceiling — beyond any real workload. |
| CDC algorithm | FastCDC v2020 | Deterministic, ~150 MB/s/core, library availability, cross-language portability. |
| CDC defaults | avg 4 MiB / min 1 MiB / max 16 MiB | Avg matches v0.2 fixed size; min/max bound chunk-count variance. |
| Erasure encoding | Reed-Solomon (10+4) default | 1.4× storage overhead at 4-loss tolerance. Industry standard for archive systems. |
| Erasure library | `reed-solomon-erasure` crate | Pure Rust, SIMD-accelerated, MIT, well-tested. |
| Resume mechanism | Implicit via BlobRef + per-chunk refcount | No explicit resume token; chunks already-present are skipped naturally. |
| Bandwidth class default | `Foreground` | Source-compat; existing callers see no behaviour change. |
| Anti-starvation threshold | 60s of zero `Background` progress | Conservative; configurable. |
| Repair trigger | Operator-explicit via `net blob repair` | Auto-repair could compound replication budget during incident windows; explicit is safer in v0.3. Auto-repair as v0.4 follow-up. |
| Wire compat strategy | v0.3 readers handle all three variants; producers downgrade to `Manifest` when targeting v0.2 receivers | No forced cluster-wide upgrade; v0.3 nodes interoperate with v0.2 nodes via downgrade. |
