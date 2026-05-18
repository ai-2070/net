# Dataforts Blob Storage — implementation plan (v0.3, terabyte-scale)

> Companion to [`DATAFORTS_BLOB_STORAGE_PLAN.md`](./DATAFORTS_BLOB_STORAGE_PLAN.md) (v0.2, shipped in v0.15). This is the **terabyte-class follow-up**: the v0.2 plan deliberately capped `BLOB_REF_MAX_SIZE` at 16 GiB and shipped a flat-manifest model. Workloads that need to address blobs larger than 16 GiB — ML model checkpoints, full-corpus snapshots, large media archives, raw scientific datasets — hit four load-bearing limits the v0.2 plan documented as deferred. This plan delivers all four lifts.

> **Revision 2** (post-review). Integrates Kyra's feedback on the first draft: lowered `TREE_THRESHOLD_BYTES`, fanout 256 → 128, hard depth cap (no tree chaining), durable partial-write recovery for multi-hour streaming uploads, strict CDC determinism spec, RS small-chunk handling via byte-striping, dynamic fetch/store window sizing, and explicit GC ↔ RS interaction invariants. See "Revision 2 changes" at the bottom for the diff against the first draft.

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

`FANOUT = 128` (a tunable but pinned in v0.3). At 4 MiB chunks and `FANOUT = 128`, a leaf addresses 512 MiB; depth 1 → 64 GiB; depth 2 → 8 TiB; depth 3 → 1 PiB; depth 4 → 128 PiB. Smaller fanout than the first draft's 256 reduces leaf manifest body size (~5 KiB vs ~10 KiB), keeps range-fetch read-amplification down (a range query that spans into the next leaf wastes less manifest read), and bounds the per-leaf chunk-count variance under CDC where chunks can be up to 4× the average size. **The `depth` field on `BlobRef::Tree` is hard-capped at 4** in v0.3 — there is no tree chaining (`Tree`-pointing-at-`Tree`) and a producer that would need depth > 4 instead hits the size-limit error. 128 PiB is well beyond any realistic single-blob workload; a future v0.4 lifts the cap if needed without wire-format change.

Wire encoding follows the existing pattern: `[4-byte magic][1-byte version=0x03][postcard-encoded body]`. The body is `(encoding, root_hash, total_size, depth)` — fixed 1 + 1 + 32 + 8 + 1 = 43 bytes regardless of blob size. `TreeNode` is itself wrapped as a `BlobRef::Small` and stored at `dataforts/blob/<hex32>` — same channel naming as v0.2 chunks, same refcount lifecycle.

Producers SHOULD emit `Tree` above `TREE_THRESHOLD_BYTES = 32 GiB`. At 4 MiB chunks, 32 GiB is 8192 chunks = ~328 KiB flat manifest body; below that, the `Manifest` path's single-round-trip simplicity beats the Tree path's two-round-trip walk. Above 32 GiB, manifest bodies grow linearly (1 TiB → ~10 MiB, 16 GiB → ~164 KiB) and the Tree path's constant ~16 KiB root + per-leaf-on-descent shape wins decisively. The threshold is an operator hint, not a wire requirement: a `Tree` of size 1 KiB is well-formed and a `Manifest` of size 16 GiB is well-formed; the threshold sets producer policy only.

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

1. **Chunker** — drains the stream into a content-defined or fixed-size chunker. Emits `(bytes, hash)` pairs as chunk boundaries are reached. Bounded buffer: `MAX_CHUNK_SIZE_BYTES = 16 MiB` (matches CDC `max`); the chunker enforces this hard cap and splits at the boundary if a content-defined cut hasn't fired by then (FastCDC's `max` constraint).
2. **Chunk store fan-out** — emitted chunks dispatch to `MeshBlobAdapter::store_chunk` with **dynamic parallelism**. The window targets ~256 MB of bytes-in-flight per stream so disk and network stay saturated without RAM pressure:
   ```
   parallelism = min(
       STORE_WINDOW_TARGET_BYTES / current_chunk_size,
       num_cpus() * 2,
       config.blob_max_store_parallelism,     // operator cap, default 64
   )
   ```
   At Fixed 4 MiB chunks → 64 in flight (256 MB / 4 MiB). At CDC averaging 4 MiB but peaking at 16 MiB → 16–64 in flight, self-adjusting. Memory ceiling: `STORE_WINDOW_TARGET_BYTES` independent of file size or chunk-size variance. Backpressure: the chunker awaits when the window is full.
3. **Cross-replica write coordination** — each `store_chunk` returns when the chunk has been accepted to the local file AND the replication coordinator has acknowledged the replicate-to-quorum target (configurable: default = `replication_factor / 2 + 1`). A slow replica in the placement set surfaces as `store_chunk` latency, which throttles the chunker via the parallelism window — backpressure propagates naturally instead of overflowing the local disk while replicas lag.
4. **Manifest tree builder** — a stack of `Vec<ChunkRef>` (leaf) and `Vec<(hash, subtree_size)>` (internal) builders. When the leaf builder reaches `FANOUT` entries, close the leaf: serialize as a `TreeNode::Leaf`, store as a `BlobRef::Small`, push `(leaf_hash, leaf_size)` onto the depth-1 builder. Cascade closure up the tree as each level fills. **Each closed leaf is checkpointed durably (see § 2.5)** before the next chunk batch flows, so a crash between leaves loses at most the in-progress leaf's chunks (which are already content-addressed on disk; the staging refcount keeps them GC-safe).
5. **End-of-stream** — close every open builder level, all the way to the root. Emit the final `BlobRef::Tree` with the root hash and total size. Convert the staging entry to a permanent ref atomically with the root commit.

Memory bound: O(`STORE_WINDOW_TARGET_BYTES` + `FANOUT × tree_depth × ChunkRef_size`). At defaults: 256 MB + 128 × 4 × ~40 bytes ≈ 256 MB peak, dominated by the in-flight chunk window. For low-RAM deployments the window target can be set to 64 MB → ~64 MB peak. **Bounded regardless of input size.**

A pre-fix `store_stream` that buffered the whole blob first cannot be retained as a default fallback for adapters that don't override — the default trait impl's 16 GiB cap is the right behaviour for adapters that genuinely accumulate a buffer. `MeshBlobAdapter::store_stream` overrides explicitly. The v0.3 trait surface adds a `chunking: ChunkingStrategy` parameter with a default that preserves the v0.2 behaviour for adapters that don't care.

### 2.5 Durable partial-write recovery (streaming staging)

A 1 TiB upload at 200 MB/s takes ~90 minutes. Process crashes, network drops, and operator-triggered restarts in that window must not force the producer to start from byte 0 — and must not corrupt the substrate's view of the partially-built blob. v0.2's "no resume" model is acceptable at 16 GiB scale because the worst case is a sub-minute re-upload; at TB+ it's load-bearing infrastructure.

**Staging-token API.** The producer opens a staging slot before streaming bytes:

```rust
impl MeshBlobAdapter {
    pub async fn begin_streaming_store(
        &self,
        chunking: ChunkingStrategy,
        encoding: Encoding,
        total_size_hint: Option<u64>,
    ) -> Result<StreamingStoreToken, BlobError>;

    pub async fn append_to_stream(
        &self,
        token: &StreamingStoreToken,
        bytes: &[u8],
    ) -> Result<(), BlobError>;

    pub async fn finalize_streaming_store(
        &self,
        token: StreamingStoreToken,
    ) -> Result<BlobRef, BlobError>;

    pub async fn abort_streaming_store(
        &self,
        token: StreamingStoreToken,
    ) -> Result<(), BlobError>;

    /// Resume after a crash. Returns the byte offset from which the
    /// producer should continue feeding bytes — already-emitted
    /// chunks are skipped on the re-stream via content-address
    /// idempotence + the staging record's completion log.
    pub async fn resume_streaming_store(
        &self,
        token: StreamingStoreToken,
    ) -> Result<ResumePoint, BlobError>;
}
```

`StreamingStoreToken` is a `[u8; 32]` opaque handle (BLAKE3 of a producer-supplied salt + start timestamp + producer node id). Persists across producer process restarts via the operator's own out-of-band storage (env var, file, secret manager) — the substrate doesn't track *who* a token belongs to, only that the token references a staging record.

**Staging record on the substrate.** Each open token has a corresponding entry at a reserved channel `dataforts/blob-staging/<token_hex>`. The record is itself a RedexFile carrying append-only checkpoints:

```rust
struct StagingCheckpoint {
    seq: u32,                          // 0-indexed; latest seq is canonical
    chunking: ChunkingStrategy,        // pinned at begin_streaming_store
    encoding: Encoding,                // pinned at begin_streaming_store
    completed_leaves: Vec<[u8; 32]>,   // closed leaf hashes, in tree order
    completed_internals: Vec<Vec<[u8; 32]>>,  // closed internal nodes per depth
    last_chunk_byte_offset: u64,       // resume cursor
    last_checkpoint_unix_ms: u64,      // staleness clock
}
```

Checkpoints land every `STAGING_CHECKPOINT_INTERVAL` (default: every leaf close, or every 60 s of streaming, whichever fires first). The append-only record makes recovery cheap: read the latest seq's checkpoint, recover the in-progress tree-builder state, resume.

**GC protection.** Every chunk stored during a staging session bumps a refcount entry tagged `Staging(token)`. The staging refcount is independent of normal blob refcounts — a chunk with only staging refs is NOT eligible for GC, but is also NOT counted toward a live `BlobRef`. On `finalize_streaming_store`, staging refs convert to normal `BlobRef::Tree` refs atomically; on `abort_streaming_store`, staging refs decrement and the chunks fall back to normal GC eligibility (after the retention floor).

**Staleness sweep.** Staging records carry a TTL (default 7 days). A daily sweep aborts any staging session whose latest checkpoint is older than the TTL, decrementing all its staging refs and releasing the staging channel. Operators get `blob_staging_aborted_total` so a wedged producer surfaces as a metric rather than silent disk leak.

**Concurrency model.** A single token is single-writer (a second `append_to_stream` with the same token errors with `Conflict`). Multiple distinct tokens can stream to the same channel concurrently — they don't interact at the staging layer.

**Resume correctness.** On `resume_streaming_store`, the producer presents the token and gets back a `ResumePoint { byte_offset, completed_chunk_count }`. The producer rewinds its source stream to `byte_offset` and feeds bytes from there. Because chunking is deterministic (Fixed: by offset; CDC: by content), the re-streamed bytes re-derive the same chunk boundaries the original run produced. `store_chunk` is already idempotent on hash collision (v0.2), so re-emitting chunks the original run already stored is a no-op. New chunks past the resume point land in the staging record's continuation.

For CDC, deterministic resume requires the producer's local CDC state at the resume point. The staging record carries the last chunker `roll_hash` value at checkpoint time so the resumer can seed the chunker mid-stream without re-scanning prior bytes. This is the only CDC-specific resume hook; FastCDC's gear-table approach lets us snapshot + restore the roll state in a fixed 8 bytes per checkpoint.

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
3. If `range` spans multiple children, fan out one descent per spanning child concurrently. The fan-out window is **dynamic**, sized the same way the store window is: target ~256 MB of bytes-in-flight per stream, divided by current chunk size, capped by `config.blob_max_fetch_parallelism` (default 64). For a typical fetch this resolves to ~64 in-flight chunks for Fixed 4 MiB, self-adjusting for CDC.
4. At the leaf, walk the `Vec<ChunkRef>` to find the spanning chunks (fixed-size: direct index; CDC: prefix-sum scan).
5. Fetch the spanning chunks (already-existing `fetch_chunk` path); slice the returned bytes to `range`.

For `BlobRef::Manifest` (v0.2): use the existing `byte_range_to_chunks` helper unchanged.

For `BlobRef::Small`: existing path unchanged.

**Manifest-fetch caching.** A per-node LRU on `[u8; 32] → TreeNode` (default 64 MiB / ~32 K leaves at fanout 128) absorbs adjacent range reads on the same blob without re-fetching the manifest path. The cache participates in the existing `BlobHeatRegistry` so hot trees pin in cache via the same data-gravity mechanism that pins hot chunks.

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

**Strict determinism specification.** Cross-language CDC determinism is load-bearing: a Node binding chunking the same TB blob as a Rust producer MUST produce byte-identical chunk boundaries, or dedup breaks and consumers can't share content across producer languages. v0.3 pins every CDC parameter:

| Parameter | Value | Rationale |
|---|---|---|
| Variant | FastCDC-2020 (normalized chunking) | Newer variant with better boundary distribution; gear table fixed. |
| Gear table | 256 × `u64` constants from `fastcdc` v3.1.0 default table | Frozen as a fixture at `net/crates/net/src/adapter/net/dataforts/blob/cdc_gear_table.bin`. Cross-language bindings load the same fixture; no per-language regeneration. |
| Hash window | 64 bytes | Standard FastCDC; smaller windows reduce boundary stability. |
| Normalization | Level 2 (NC = 2) | Tighter boundary distribution around `avg`; standard for production CDC. |
| Mask shifts | `mask_s = avg.log2() + NC`, `mask_l = avg.log2() - NC` | Derived from `avg`, deterministic. |
| Byte order | Little-endian throughout | Pinned for cross-platform consistency; gear table loaded LE. |
| Min-cut enforcement | Hard reject any chunk below `min` (advance past min before considering cut) | Standard. |
| Max-cut enforcement | **Hard cut at `max`** regardless of content. The `fastcdc` crate's `cut()` method enforces this; v0.3 wraps it with an assertion in debug builds so a future library bug surfaces immediately. | Without this, pathological inputs (long runs of zeros, etc.) can produce 100+ MiB chunks that blow past the manifest's `ChunkRef::size: u32` bound or the in-memory chunk buffer. |
| Resume snapshot | 8-byte roll-hash state at checkpoint | Lets `resume_streaming_store` re-seed the chunker mid-stream without re-scanning prior bytes. |

A conformance fixture lives at `tests/cdc_determinism.rs` with three input shapes (random bytes, mostly-zeros with sparse non-zero runs, alternating-byte patterns) and pinned expected boundary positions. Every binding (Rust core, Node, Python, future Go) MUST pass this fixture before its CDC implementation is admitted. A mismatch fails the binding's CI, not just an integration test.

CDC chunks store via the same `dataforts/blob/<hex32>` path. The `ChunkRef::size` field (already a `u32`) carries the actual chunk size — variable per chunk under CDC, constant under Fixed. Leaf nodes carrying CDC chunks use the same wire shape as fixed-chunk leaves; the reader differentiates only at range-fetch time (prefix-sum scan vs direct index).

**Per-channel selection.** The channel's `RedexFileConfig::chunking` (new field) pins the producer's strategy at store time. Consumers don't care about the producer's strategy at all — they read `ChunkRef::size` from the leaf and slice accordingly. A blob stored with CDC and a blob stored with Fixed can coexist on the same channel; the strategy is only relevant at chunking time.

**Migration of v0.2 blobs.** A v0.2 `BlobRef::Manifest` is implicitly Fixed-chunked (all chunks at 4 MiB, except the last). v0.3 readers handle both `Manifest` and `Tree` paths; producers should not re-chunk existing v0.2 blobs (that would invalidate every consumer's chunk-hash cache).

### 6. Reed–Solomon erasure coding

The v0.2 `Encoding` enum already reserves `ReedSolomon { k, m }`. v0.3 implements it:

**Store path.** When a producer publishes with `Encoding::ReedSolomon { k, m }`:
1. Chunker emits chunks normally.
2. The striper accumulates emitted chunks into a stripe **by data bytes, not by chunk count**. A stripe closes when accumulated data reaches `RS_STRIPE_TARGET_BYTES = k × avg_chunk_size` (default: `10 × 4 MiB = 40 MiB` of data). Under CDC this typically holds 8–12 chunks; under Fixed it's exactly `k`. Striping by bytes is the v0.3 answer to the CDC-RS small-chunk explosion: a pathological mix of tiny + huge chunks no longer creates parity blowup because the stripe boundary is content-size-driven, not chunk-count-driven.
3. Within a closed stripe, **data chunks are padded** to the size of the largest data chunk in the stripe (zero-padding) before parity computation. RS requires equal-sized inputs; padding bytes are zero, and on decode the manifest's `ChunkRef::size` carries the actual chunk size so the substrate slices correctly. Padding overhead is bounded by stripe variance — for fixed chunks it's zero; for CDC with min=1 MiB / max=16 MiB it can reach ~50% per-stripe in pathological cases, but the striper's byte-target keeps the absolute waste bounded at ~`RS_STRIPE_TARGET_BYTES × m / k`.
4. Parity chunks: `m` chunks computed via systematic Reed-Solomon over `GF(2^8)`, each sized to the post-padding data chunk size. Parity chunks have their own BLAKE3 hashes and store at `dataforts/blob/<hex32>` like any other chunk.
5. The leaf `TreeNode` lists all `k + m` chunks of the stripe in order (data chunks then parity chunks) with each `ChunkRef` carrying a new `role: ChunkRole` field.

```rust
pub enum ChunkRole {
    Data,
    Parity { stripe_index: u8 },
}

pub struct ChunkRef {
    pub hash: [u8; 32],
    pub size: u32,          // actual (pre-padding) data size
    pub role: ChunkRole,    // new in v0.3 — see Open Question 1
}
```

**Small-stripe fallback.** A stripe that hasn't reached `RS_STRIPE_MIN_BYTES = 8 MiB` of accumulated data at end-of-stream (i.e., the blob is so small that it doesn't fill a full stripe) **falls back to `Encoding::Replicated`** for that final stripe. The leaf records the fallback in a per-stripe `encoding_override` field; the fetch path consults it before attempting RS reconstruction. This avoids a 1 MiB blob from incurring 4 MiB of parity overhead (5× storage cost for an RS-(10,4) blob whose data is half a chunk). The full-stripe path is unaffected.

Defaults: `k = 10, m = 4` (1.4× storage overhead, tolerates any 4 chunk losses per stripe). Other configurations: `(6, 3)` for 1.5× / 3-loss; `(20, 4)` for 1.2× / 4-loss; `(3, 2)` for 1.67× / 2-loss in small-cluster deployments. The wire `k, m: u8` allows up to `(255, 255)`; v0.3 rejects `k + m > 255` at the producer-side validator and warns on `k + m > 64` (most RS libraries tune for the smaller range).

**Fetch path.**
1. Resolve the leaf for the range. Identify the stripe.
2. Optimistic: fetch the `k` data chunks of the stripe in parallel. If all succeed, slice and return.
3. Reconstruction: on any data-chunk fetch failure (`NotFound`, `HashMismatch`, `ShortChunk`, `Cancelled`), fetch enough parity chunks to make the total available count ≥ `k`. Reconstruct the missing data chunk(s) via the inverse RS matrix.
4. Failure: if more than `m` chunks of the stripe are missing (combined data + parity), return `BlobError::Backend("erasure: stripe N unrecoverable, {} chunks lost")`.

Reconstruction reads the missing-chunk slice into a fresh buffer and returns it to the caller, but does NOT auto-restore the missing data chunk on disk by default. A separate **repair sweep** (operator-triggerable; opt-in scheduled at `RedexFileConfig::blob_repair_cadence`) walks reachable stripes, detects degraded ones (< `k` data chunks present), and stores reconstructed data chunks back to their content-addressed path. Without auto-restore, every fetch on a degraded stripe re-computes the reconstruction — fine for cold blobs, not great for hot ones. The repair sweep closes the loop.

**Per-blob selection.** The blob's `BlobRef::Tree::encoding` pins the encoding for every full stripe in that blob; the small-stripe fallback only affects the trailing partial stripe. A `Replicated` blob and a `ReedSolomon` blob coexist on the same channel — encoding is a property of the BlobRef, not the channel.

**RS library.** `reed-solomon-erasure` crate (MIT, pure Rust, SIMD-accelerated on x86-64 / aarch64). Performance: ~3 GB/s for `(10, 4)` encoding on a single core; reconstruction is similar throughput. The conformance suite pins encode/decode against fixture vectors so cross-version compatibility holds across substrate versions.

#### 6.1 GC + RS interaction invariants

Reed-Solomon adds the first case in v0.3 where a chunk's lifecycle depends on the lifecycle of OTHER chunks (its stripe-mates). v0.2's chunk refcount was independent: a chunk is deletable iff its refcount = 0 AND retention floor passed. v0.3 layers stripe-aware rules on top:

**Refcount semantics — uniform.** Data chunks AND parity chunks carry refcount entries in `BlobRefcountTable` the same way. A `BlobRef::Tree` referencing a stripe bumps refcount on every member chunk (data + parity) at fold time; a deref decrements every member. **Parity chunks are NOT auxiliary metadata; they are full first-class GC participants.** This is necessary so that a parity chunk required for an active blob can't be silently swept just because its data sibling isn't being fetched right now.

**Sweep rule — degraded-stripe pin.** Before sweeping a chunk, the GC checks: does this chunk belong to any stripe where the stripe is currently degraded (< `k` available data chunks)? If yes, the chunk is **pinned against GC** until either:
- the stripe is repaired (back to `k` data chunks available), OR
- the stripe is fully dereferenced (refcount of every member drops to 0).

Without this pin, the GC could sweep the only remaining parity chunk of a degraded stripe and turn a recoverable degradation into an unrecoverable loss. The check costs O(1) per chunk via a new `stripe_membership` index keyed by chunk hash; the index is built incrementally as stripes are written and torn down as stripes are dereferenced.

**Repair-sweep refcount semantics.** When the repair sweep reconstructs a missing data chunk and re-stores it, the repair path bumps the chunk's `last_seen_unix_ms` to now (so the retention-floor clock restarts — the chunk is "newly observed" again). The stripe's degraded-pin lifts automatically as the chunk count reaches `k`.

**Stripe dereference semantics.** When the last `BlobRef::Tree` referencing a stripe is GC'd, every stripe member (data + parity) drops to refcount 0 simultaneously. The retention floor then applies uniformly across the stripe. A blob being deleted doesn't leave orphan parity hanging around.

**Cross-stripe dedup.** Two different `BlobRef::Tree` blobs that include identical content can share data chunks (chunks are content-addressed). But if blob A uses `Encoding::Replicated` for chunk X and blob B uses `Encoding::ReedSolomon` and chunk X is a data member of a stripe in B, the SAME chunk X is referenced by both — its refcount is the sum. **Parity chunks of B's stripe are unique to B** (they're derived from B's specific stripe membership); two blobs that share data chunks do NOT share parity. This is a property of systematic RS: parity is a function of the specific data-chunk set, not just the data content.

**Operator-visible counters.**
- `blob_stripe_degraded_gauge` — number of stripes currently in degraded state (< `k` data chunks but ≥ 1 parity available).
- `blob_stripe_unrecoverable_gauge` — number of stripes where data + parity available < `k` (operator action required).
- `blob_chunks_pinned_by_degraded_stripe_total` — count of GC sweeps that skipped a chunk due to degraded-stripe pin.
- `blob_repair_chunks_restored_total` — count of chunks re-stored by the repair sweep.

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

**Cluster-wide RS fencing (operator workflow).** Because an RS blob is unreadable by any peer in the placement set that doesn't advertise `dataforts:blob-erasure-supported`, the first attempt to publish an RS blob on a cluster MUST be gated on a pre-flight check:

1. **Discovery.** `MeshBlobAdapter::publish_with_blob(.., Encoding::ReedSolomon { k, m })` consults the destination channel's current `PlacementFilter` to enumerate the placement set, then queries each member's advertised capability tags.
2. **All-or-nothing gate.** If ANY member in the placement set lacks `dataforts:blob-erasure-supported`, the publish fails with `BlobError::Backend("encoding ReedSolomon requires all placement-set nodes to advertise dataforts:blob-erasure-supported; missing: [<node_ids>]")`. The producer is told exactly which nodes need upgrading.
3. **Operator override.** `--encoding-force` flag on `net blob publish` (and a matching `ReedSolomonPolicy::ForceMixed` enum on the API) lets an operator publish anyway, accepting that some replicas will be unreachable. Logged at warn with an explicit "you are reducing durability" message.
4. **Channel-level flip.** An operator that wants RS-by-default on a channel sets `RedexFileConfig::default_encoding = Encoding::ReedSolomon { k, m }`. This config setting itself runs the same pre-flight check on first apply; a mixed-version cluster rejects the config change until uniform.
5. **Watcher integration.** RedEX channel watchers consult the encoding field on every event-fold; a watcher running on a v0.2-only node observes an RS blob and emits a structured warn (`dataforts:blob-encoding-unsupported`) rather than silently producing a `HashMismatch` on data chunks misinterpreted as raw bytes.

This mirrors how operators roll out other "all-or-nothing" capability changes (e.g. capability-tag widening in v0.13). It's an operator workflow more than a wire-format guarantee — there's no automatic detection across the entire cluster, only across each placement set. The placement set is the unit of upgrade.

**Rollback.** An operator that needs to revert from RS to Replicated for a specific blob runs `net blob reencode <ref> --encoding replicated`. The substrate fetches the RS blob (reconstructing as needed), re-stores it under the new encoding, returns the new BlobRef. The old RS BlobRef remains valid (and continues to consume storage) until its refcount drops; operators wanting space recovery decref the old ref via `net blob deref <old_ref> --tree`.

### 10. Performance + correctness targets

| Surface | v0.2 | v0.3 target |
|---|---|---|
| Max addressable blob size | 16 GiB | 128 PiB (depth 4 × fanout 128 × 4 MiB chunk) |
| `store_stream` peak memory | ~size of blob | ~256 MB (dominated by in-flight chunk window) |
| Range-fetch latency at 1 TiB scale | ~10 MiB manifest + 1 chunk fetch | ~10 KiB × depth manifest + 1 chunk fetch (~30 KiB total) |
| Manifest-path bytes for 1 TiB | ~10 MiB | ~30 KiB (depth 2: root 5 KiB + leaf 5 KiB + working slack) |
| Storage cost at `replication_factor=3` | 3× | 1.4× (RS 10+4 full stripes), 3× (small-stripe fallback) |
| Cross-revision dedup (small edits) | 0 % (Fixed) | ~99 % (CDC, single-chunk delta) |
| Resume effectiveness on 50% partial transfer | 0 % (no resume) | 100 % (staging token + chunk-address idempotence; receiver re-streams from the resume offset, all prior chunks dedup) |
| TB-scale `store_stream` throughput | N/A (cap) | ≥ 200 MB/s on a 1 Gbps link |
| Manifest cache memory footprint | N/A | ≤ 64 MiB default, configurable |
| Streaming-store crash survival | Lose entire upload | Lose ≤ checkpoint interval (default ≤ 60s of bytes) |

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

1. **Manifest tree fan-out tuning.** Fanout 128 is a balanced default for chunk=4 MiB; the actual optimal depends on chunk count distributions in real workloads. Too-low fanout → deeper trees → more manifest fetches per range; too-high fanout → larger leaves → more bytes wasted on partial range reads. v0.3 ships fanout as a tunable in `RedexFileConfig` with default 128; a follow-up could auto-tune based on observed access patterns.

2. **CDC boundary stability across implementations.** FastCDC has multiple variants in the wild (`v2017`, `v2020`, gear table choice). v0.3 pins to FastCDC-2020 + a frozen gear-table fixture binary committed to the tree; cross-language bindings load the same fixture. The conformance suite enforces byte-identical boundaries across three input shapes for every binding before admission. Residual risk: a future `fastcdc` crate update that silently changes its `cut()` semantics; mitigated by version-pinning the dependency and re-running the conformance fixture in CI.

3. **Erasure reconstruction latency.** On a degraded stripe, `(10, 4)` reconstruction fetches up to 14 chunks (10 data + up to 4 parity) and runs the inverse-matrix decode. Fetch RTT × 14 + decode CPU = real latency. Mitigated by: hot blobs trigger the repair sweep so degraded stripes don't stay degraded; cold blobs are reconstructed once per fetch (acceptable at TB scale where fetches are themselves rare); the degraded-stripe pin (§ 6.1) ensures the substrate doesn't GC the parity chunks reconstruction depends on.

4. **Cache invalidation for hot blob trees.** A manifest cache keyed on root_hash never needs invalidation (content-addressed), but a hot blob whose tree exceeds the cache size churns the cache on every fetch. Mitigated by: cache size is configurable; large trees touch many leaves so the LRU's natural eviction handles the worst case; data-gravity pinning prevents hot trees from evicting under cold-tree pressure.

5. **Bandwidth class starvation under sustained background load.** A `Background` stream blocked by a steady stream of `Foreground` fetches never makes progress until the 60 s hatch fires. For multi-TB backfills this could mean the backfill never finishes if the cluster is under sustained interactive load. Mitigated by the hatch (60 s isn't unreasonable for a TB-scale job); operators can configure a tighter hatch or pin the backfill to `Foreground` for known-long jobs.

6. **Repair sweep load.** Walking every reachable `BlobRef::Tree` to detect degraded stripes is O(N chunks) per sweep. At a million chunks, that's a real chunk-stat I/O load. Mitigated by sweep cadence being operator-configurable (default 24 h), and by using the existing refcount table + `stripe_membership` index for the walk so no fresh disk reads are needed for the detection phase.

7. **Wire-format churn.** v0.3 adds one wire variant (`0x03`) and a new field on `ChunkRef` (`role`). v0.3 takes Option A (the recommendation from Open Question 1 in the first draft): bump leaf encoding to a v0.3-only `TreeNode::Leaf` with `Vec<ChunkRefV3>` carrying `role`; `ManifestBody::chunks` (v0.2 leaves) stays the legacy shape. Decoders branch on the wire version; no postcard ambiguity.

8. **Staging-record orphan accumulation.** A producer that crashes mid-stream and never resumes accumulates a staging record + pinned chunks until the 7-day TTL fires. In a long-running deployment with many such crashes, the staging channel grows monotonically until sweep. Mitigated by: the staging channel is a real RedexFile that participates in normal sweep; orphan staging records show up in `blob_staging_aborted_total`; operators alert on the growth rate. A tighter TTL (e.g. 24 h) is an operator-configurable knob for deployments where 7 days is too forgiving.

9. **RS small-stripe storage cost.** Blobs smaller than `RS_STRIPE_MIN_BYTES = 8 MiB` fall back to `Replicated` for their trailing stripe, so a 1 MiB blob published with `Encoding::ReedSolomon` actually stores at 3× (Replicated) overhead, not 1.4×. Documented in the encoding semantics — operators publishing small blobs should use `Replicated` explicitly, or accept the fallback. Mitigated by `blob_stripe_fallback_total` metric so operators see how often this fires; auto-warn at publish time if `total_size < RS_STRIPE_MIN_BYTES`.

---

## Open questions

**Resolved by Revision 2.** The first draft listed five open questions; Revision 2 closes four:

- ~~`ChunkRef::role` wire shape~~ → **Decision: Option A** (new `ChunkRefV3` struct in `TreeNode::Leaf`; v0.2 `ManifestBody::chunks` unchanged). Wire-format clean break, no postcard ambiguity.
- ~~`TREE_THRESHOLD_BYTES`~~ → **Decision: 32 GiB** (Tree wins decisively above ~1 MB manifest body, which is ~25 GiB at fixed 4 MiB chunks; 32 GiB rounds up with margin).
- ~~Tree fanout~~ → **Decision: 128** (leaf 512 MiB at fixed chunks; bounded leaf size under CDC variance; 128 PiB ceiling at depth 4).
- ~~Tree depth cap chaining~~ → **Decision: hard cap 4, no chaining**. 128 PiB ceiling is well beyond any plausible workload; lifting the cap later is non-breaking.

**Still open in Revision 2:**

1. **CDC parameters.** `avg = 4 MiB, min = 1 MiB, max = 16 MiB` is the default. Workloads with mostly-small payloads (logs, audit events) might prefer `avg = 256 KiB` for finer dedup; ML checkpoints might prefer `avg = 32 MiB` for fewer chunks per blob. Ship presets (`ChunkingStrategy::cdc_for_workload(WorkloadHint::Logs)`) or raw `avg/min/max`? **Recommendation: raw params + docs; presets are easy to add later.**

2. **Erasure-coded chunk channel naming.** Data + parity chunks both live at `dataforts/blob/<hex32>` (uniform GC / replication / refcounting). Operators auditing a chunk file can't tell from the name whether it's data or parity. Should parity chunks live at `dataforts/blob-parity/<hex32>` to disambiguate at the filesystem level? **Recommendation: keep uniform — operators query the parent `Tree`'s leaf to learn the role; the metric counters expose the role at the observability layer.**

3. **Per-channel default encoding.** Should `RedexFileConfig` carry a default `Encoding` (so all blobs published to a channel inherit RS without per-call opt-in)? Cleaner UX, but operators may want some blobs replicated and others erasure-coded on the same channel. **Recommendation: per-call only in v0.3; per-channel default as v0.4 follow-up if a workload demands it.** (Acknowledged: the `Cluster-wide RS fencing` section already references `default_encoding` for operator convenience; this open question is really about whether v0.3 ships that knob in Phase C or defers to v0.4.)

4. **Manifest cache shared with greedy LRU.** The proposed manifest cache is separate from the greedy LRU cache. A combined cache would unify the eviction model but complicate the cache's value type (chunks vs tree nodes). **Recommendation: separate cache in v0.3; merge in a follow-up if dual-cache memory pressure shows up in production.**

5. **Staging-record TTL default.** The 7-day default for stale staging records balances "give operators time to recover from multi-day outages" against "don't leak chunks indefinitely." Workloads with reliable producers and tight disk budgets might prefer 24 h. **Recommendation: 7 days default, configurable per channel via `RedexFileConfig::blob_staging_ttl`. Document the trade in operator runbook.**

6. **Auto-repair on degraded-stripe detection.** v0.3 ships repair as operator-explicit (`net blob repair`). A future v0.4 could auto-trigger repair from the GC tick when a degraded stripe is observed, IF disk pressure is below the critical threshold AND the replication budget has headroom. **Recommendation: defer to v0.4; v0.3 ships the metric (`blob_stripe_degraded_gauge`) so operators can build their own auto-repair via the CLI in the meantime.**

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
| Wire ceiling lift mechanism | Hierarchical manifest tree | One additional wire variant; lifts cap from 16 GiB to 128 PiB. Alternative (raise `BLOB_REF_MAX_SIZE`) keeps the flat-manifest read-cost wall. |
| Tree fanout | 128 | Smaller leaf (512 MiB) than fanout-256 alternative; bounds leaf size under CDC variance; range-fetch wastes less manifest. Revision-2 change. |
| Max tree depth | 4 hard cap, no chaining | 128 PiB ceiling at fanout 128 — beyond any real workload. Lifting later is non-breaking. Revision-2 change. |
| `TREE_THRESHOLD_BYTES` | 32 GiB | Below threshold, flat `Manifest` (manifest body < 1 MB) is cheaper; above, Tree path wins decisively. Revision-2 change (was 256 GiB in first draft). |
| `ChunkRef::role` wire shape | Option A: new `ChunkRefV3` in `TreeNode::Leaf` | v0.2 `ManifestBody::chunks` unchanged; clean version-gated decode. Revision-2 decision (was open question). |
| CDC algorithm | FastCDC-2020 + frozen gear-table fixture | Deterministic, ~150 MB/s/core, library availability, cross-language portability. Fixture binary pinned in repo for binding parity. |
| CDC defaults | avg 4 MiB / min 1 MiB / max 16 MiB / NC=2 | Avg matches v0.2 fixed size; min/max bound chunk-count variance; max enforced by hard cut at chunker. |
| Erasure encoding | Reed-Solomon (10+4) default | 1.4× storage overhead at 4-loss tolerance. Industry standard for archive systems. |
| Erasure library | `reed-solomon-erasure` crate | Pure Rust, SIMD-accelerated, MIT, well-tested. |
| RS striping | By bytes (`RS_STRIPE_TARGET_BYTES = k × avg_chunk_size`) | Stripe-by-chunk-count under CDC creates small-chunk-explosion + parity waste. Byte-striping bounds per-stripe parity overhead. Revision-2 change. |
| RS small-stripe fallback | Below `RS_STRIPE_MIN_BYTES = 8 MiB`, fall back to `Replicated` | Avoids 5×+ storage cost on sub-stripe blobs. Per-stripe `encoding_override` records the fallback. Revision-2 change. |
| GC ↔ RS interaction | Degraded-stripe pin via `stripe_membership` index | Prevents GC from sweeping a parity chunk required for an active degraded stripe. Revision-2 explicit invariant. |
| Streaming durability | Staging-token API + checkpoint record | Multi-hour uploads survive process crashes; resume from last checkpoint, not byte 0. Revision-2 addition. |
| Store/fetch parallelism | Dynamic window targeting 256 MB bytes-in-flight | Hard-coded constants don't scale for TB+ blobs. Revision-2 change (was fixed 16/8). |
| Cross-replica backpressure | `store_chunk` returns after quorum ack | Slow-replica detection propagates through parallelism window; no local-disk overflow while replicas lag. Revision-2 addition. |
| Resume mechanism | Staging token + per-chunk refcount idempotence | Explicit token for operator-recoverable resume; implicit chunk dedup for the re-stream. Revision-2 strengthening. |
| Bandwidth class default | `Foreground` | Source-compat; existing callers see no behaviour change. |
| Anti-starvation threshold | 60 s of zero `Background` progress | Conservative; configurable. |
| Repair trigger | Operator-explicit via `net blob repair` | Auto-repair could compound replication budget during incident windows; explicit is safer in v0.3. Auto-repair as v0.4 follow-up. |
| Wire compat strategy | v0.3 readers handle all three variants; producers downgrade to `Manifest` when targeting v0.2 receivers | No forced cluster-wide upgrade; v0.3 nodes interoperate with v0.2 nodes via downgrade. |
| RS cluster fencing | All-or-nothing pre-flight per placement set | Mixed-version clusters reject RS publish (with operator override flag); placement-set is the unit of upgrade. Revision-2 addition. |

---

## Revision 2 changes (diff vs first draft)

Kyra's review of the first draft surfaced two math/threshold refinements and four substantive additions. Captured here so a reader who only has the first draft can see exactly what moved:

**Numeric tuning:**
- `TREE_THRESHOLD_BYTES`: 256 GiB → **32 GiB**. (Kyra's "256 MB manifest at 256 GiB" was off by ~80× — actual ~2.6 MB at ~40 bytes/`ChunkRef` — but the direction stands: Tree path uniformly beats Manifest above ~25 GiB.)
- `FANOUT`: 256 → **128**. Smaller leaves (512 MiB vs 1 GiB), bounded under CDC variance, less range-fetch waste.
- Max addressable size revised: ~16 EiB at fanout 256 → **128 PiB at fanout 128**. Still well beyond any plausible workload.
- Tree depth chaining removed: **hard cap at 4, reject deeper**. No `Tree`-pointing-at-`Tree`.

**Substantive additions:**
- **§ 2.5 Durable partial-write recovery** — new section. Staging-token API, append-only checkpoint record, GC-protecting `Staging(token)` refcount tag, 7-day staleness sweep, CDC roll-hash snapshot for resume continuity.
- **§ 5 strict CDC determinism spec** — pinned variant (FastCDC-2020), gear-table fixture committed to the tree, hard-cut max enforcement, conformance fixture vectors enforced by binding CI.
- **§ 6 RS striping by bytes + small-stripe fallback** — was striping-by-chunk-count, vulnerable to CDC small-chunk explosion. Now `RS_STRIPE_TARGET_BYTES = k × avg_chunk_size`; below `RS_STRIPE_MIN_BYTES = 8 MiB`, fall back to `Replicated`.
- **§ 6.1 GC ↔ RS interaction** — new subsection. Degraded-stripe pin via `stripe_membership` index; parity = data for refcount purposes; cross-stripe dedup only on data, never on parity.

**Operational refinements:**
- `STORE_PARALLELISM = 16` → **dynamic window targeting 256 MB bytes-in-flight**, capped by `config.blob_max_store_parallelism` (default 64).
- `FETCH_PARALLELISM = 8` → same dynamic shape.
- **Cross-replica backpressure** — `store_chunk` returns only after quorum ack; slow-replica latency propagates as throttle.
- **CDC max enforcement** — explicit hard cut at `max`, assertion in debug builds, so a pathological input (long zero runs, etc.) can't blow past the `ChunkRef::size: u32` bound.

**Migration hardening:**
- **Cluster-wide RS fencing** — pre-flight check of all `dataforts:blob-erasure-supported` tags in the placement set before first RS publish. Operator override flag (`--encoding-force`) accepts the durability reduction explicitly. Channel-level encoding flip runs the same check.
- **Rollback path** — `net blob reencode` to migrate a blob from RS back to Replicated.

**Open questions resolved (4 of 6 from first draft):**
- `ChunkRef::role` wire shape → Option A (clean break).
- `TREE_THRESHOLD_BYTES` → 32 GiB.
- Tree fanout → 128.
- Tree depth cap chaining → no chaining, hard cap 4.

**New open questions (2):**
- Staging-record TTL default (7 days suggested; configurable).
- Auto-repair on degraded-stripe detection (deferred to v0.4; v0.3 ships the metric so operators can build their own).

**Updated LOC estimate.** The four substantive additions add ~1500 LOC across:

| Surface (additions vs first draft) | Net LOC |
|---|---|
| `blob_staging.rs` (new — token API, checkpoint record, sweep) | +700 |
| `cdc.rs` (gear-table fixture binary loader + conformance harness) | +150 |
| `erasure.rs` (byte-striping + small-stripe fallback) | +250 |
| `refcount.rs` (`stripe_membership` index + degraded-stripe pin) | +200 |
| `mesh.rs` (cluster-fencing pre-flight + reencode path) | +200 |
| Tests + conformance | +500 |

Revised total: **~6,650 LOC** (~35% of v0.2's footprint).
