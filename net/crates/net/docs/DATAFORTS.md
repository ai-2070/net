# Dataforts

The compositional data layer above the transport substrate: speculative
caching, read-driven placement, content-addressed blob storage, and directory
transfer. Everything here is gated behind the `dataforts` Cargo feature.

Dataforts is deliberately **not** a storage engine. Each phase composes
existing substrate primitives — the tag taxonomy, the capability index, the
replication runtime, the placement filter — rather than adding a parallel
mechanism. That constraint is why there is no migration engine: chains move
because advertisement plus pull produce movement, not because something
orchestrates it.

Design plans and locked decisions:
[`DATAFORTS_PLAN.md`](../../../../docs/internal/plans/DATAFORTS_PLAN.md),
[`DATAFORTS_BLOB_STORAGE_PLAN.md`](../../../../docs/internal/plans/DATAFORTS_BLOB_STORAGE_PLAN.md),
[`DATAFORTS_BLOB_OVERFLOW_PLAN.md`](../../../../docs/internal/plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md).

## The four surfaces

| Module | What it does |
|---|---|
| `greedy` | Per-node speculative caching of in-scope chains seen on the tail-subscription path |
| `gravity` | Read-rate heat counters that bias where the greedy layer pulls |
| `blob` | Content-addressed storage behind a pluggable backend, with chunking, erasure coding, refcounts and overflow |
| `dir` | Whole-directory transfer over the scheduled stream transport |

## Greedy caching

A node watches streams flowing past. It caches a copy when **all** of the
following hold: it has spare disk, ACL access, a scope match, the capability
set to fulfil the chain's `metadata.intent`, and — optionally — a colocation
hint matching a chain it already holds. When disk fills, LRU evicts.

The important part is what happens on eviction: the node **withdraws the
`causal:` capability tag**, so reads route elsewhere. Cache membership and
discovery are the same mechanism, which is why a stale cache can't serve a
read it can no longer satisfy.

Key types: `GreedyRuntime`, `GreedyConfig`, `GreedyCacheRegistry`,
`AdmissionVerdict` / `AdmitRejectReason` for the decision, and
`GreedyObserver` for instrumentation. Config validation and the admission
decision are pure functions (`greedy/admission.rs`), separable from the async
runtime that drives them.

## Data gravity

Once chains are advertised as capability tags and pulled within scope, gravity
closes the loop: per-chain read-rate counters with exponential decay, throttled
emission of `heat:<hex>=<rate>` tags onto the **existing** capability
announcement path, and a preference function the greedy runtime consults to
weight pulls by heat × scope-match × proximity-rank.

Cold chains evict first under LRU pressure; hot chains accumulate near the
readers driving the heat. No separate migration engine exists, by design — two
primitives compose into the property.

Key types: `HeatCounter`, `HeatRegistry`, `BlobHeatRegistry`,
`DataGravityPolicy`, `HeatEmission` / `EmissionDecision` for throttling, and
the `HeatSink` / `BlobHeatSink` traits.

> Heat tags are provenance-gated at the receive boundary: a node advertising
> `heat:X` without also advertising `causal:X` has the heat tag dropped. Per-peer
> rate-limiting of emissions is a known gap, tracked in the Dataforts review.

## Blob storage

Event payloads above the inline threshold ship as a `BlobRef` pointer plus a
separate fetch path. The substrate owns **hash verification (BLAKE3)** and the
discriminator byte separating inline from blob-ref payloads. Lifecycle —
refcounts, GC, retention — is delegated to the backing store by explicit
locked decision, so S3 lifecycle policies and IPFS pinning stay in charge of
their own data.

`BlobRef` has two shapes:

- **`Small`** — a single blob: a version byte, an adapter-routed URI
  (`s3://`, `ipfs://`, `file:///`, `mesh://`), and the BLAKE3-256 hash of the
  bytes the URI resolves to.
- **`Manifest`** — a chunk list for content that was split, carrying an
  `Encoding` that says how to reassemble.

The scheme in the URI picks the adapter; everything after it is opaque
passthrough. The hash is verified on every successful fetch, so an adversarial
adapter can't fake verification.

### Backends

`BlobAdapter` is the trait. Shipped implementations: `FileSystemAdapter`,
`MeshBlobAdapter` (backed by the mesh's own RedEX replication, so a
Dataforts-enabled cluster has a working content-addressed store the moment
`Redex::enable_replication(mesh)` is called), and `NoopAdapter`.
`BlobAdapterRegistry` routes by URI scheme.

### Chunking and redundancy

| Concern | Type | Notes |
|---|---|---|
| Content-defined chunking | `CdcStreamChunker`, `CdcParams` | FastCDC, wrapped for incremental async feeding rather than one-pass |
| Chunk metadata | `ChunkRef`, `ChunkRefV3`, `ChunkRole` | |
| Erasure coding | `RsEncoder`, `RsStriper`, `RsParams`, `StripeBlock` | Reed-Solomon; the replicated path is the default and gets redundancy from cross-node replication instead |
| Stripe bookkeeping | `StripeRecord`, `StripeMembershipIndex`, `RepairReport` | |
| Trees | `TreeBuilder`, `TreeNode`, `TreeNodeCache` | |

`ChunkingStrategy` and the `*SupportProbe` enums (`CdcSupportProbe`,
`ErasureSupportProbe`, `TreeSupportProbe`, `BandwidthClassSupportProbe`) let a
peer negotiate down to what both sides understand rather than failing.

### Publishing atomically

Storing and then publishing by hand races: a consumer can receive the reference
before the bytes are durable. `publish_with_blob` pins the ordering and returns
a `PublishWithBlobReceipt`.

`BlobDurability` chooses how hard the store commits before the event goes out —
`BestEffort` or `DurableOnLocal`. **A retry must keep the same durability.**
Dropping `DurableOnLocal` to `BestEffort` after a partial sync publishes an
event whose consumer can race the substrate's flush of the remaining chunks.

If the store succeeds and only the mesh publish fails, the error is
`BlobError::Backend` but the blob **is** stored and durable — republish with
`MeshNode::publish` using the receipt's `blob_ref.encode()`, rather than
storing again.

### Refcounts, overflow and migration

`BlobRefcountTable` / `RefcountEntry` track local references.

**Overflow** (`BlobOverflowController`, `OverflowConfig`, `OverflowPush`,
`OverflowPushSink`) is disabled by default and takes one boolean to enable.
When active, a node pushes its coldest blobs to overflow-enabled peers with
free disk over a dedicated nRPC. `OverflowVerdict` / `OverflowReject` /
`OverflowPushAck` carry the negotiation.

**Migration** (`BlobMigrationController`, `BlobMigrationCandidate`,
`MigrateBlobVerdict`) relocates blobs between nodes on the same tick model —
`drive_blob_migration_tick` produces a `BlobMigrationTickReport`.

Both are tick-driven and observable rather than fire-and-forget: the report
types exist so an operator can see what a tick decided and why.

## Directory transfer

`store_dir` walks a tree, turning every file into one or more content-addressed
blobs plus a single **directory manifest** blob recording the tree shape —
relative paths, modes, symlinks, and each file's `BlobRef`. `fetch_dir` pulls
the manifest, then every leaf, over the reliable scheduled stream transport,
and reconstructs the tree on disk.

Unlike blob reads generally, directory transfer fetches from a **known
source** — no per-chunk discovery. The receiver already knows which peer holds
the tree, so each chunk goes to that peer via
`MeshNode::transfer_fetch_chunk`. Types: `DirManifest`, `DirEntry`,
`EntryKind`, `DirStats`, `DirError`.

## Transfer plumbing

`BlobTransferEngine` and `BlobTransferClient` move bytes; `TransferRpcHandler`
serves the requests. `TransferHeader`, `TransferControl`, `ChunkRangeRequest`
and `TransferStatus` are the wire shapes; `TransferClientError` /
`TransferRpcError` the failures. Bandwidth is budgeted per transfer
(`blob/bandwidth.rs`) so a bulk pull can't starve the fair scheduler.

## Metrics

Each layer exposes a snapshot type rather than raw counters:
`GreedyMetricsSnapshot` (with per-channel and per-cluster breakdowns),
`BlobMetricsSnapshot`, `OverflowMetricsSnapshot`. `EvictionSweep` and
`EvictedEntry` describe what LRU actually removed on a pass.

## Source files

| Path | Purpose |
|---|---|
| `dataforts/greedy/` | Admission decision, cache registry, async runtime, config, metrics |
| `dataforts/gravity/` | Heat counters, decay, emission policy, sinks |
| `dataforts/blob/` | `BlobRef`, adapters, CDC, erasure, stripes, refcounts, overflow, migration, transfer |
| `dataforts/dir.rs` | `store_dir` / `fetch_dir` and the directory manifest |

## See also

- [`STORAGE_AND_CORTEX.md`](STORAGE_AND_CORTEX.md) — the single-node log, fold and query stack Dataforts sits above
- [`BEHAVIOR.md`](BEHAVIOR.md) — the capability index and tag taxonomy this composes against
- [`TRANSPORT.md`](TRANSPORT.md) — the stream transport blob and directory transfer ride on
- The user-facing guide: <https://ai2070.net/docs/guides/dataforts>
