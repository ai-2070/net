//! `BlobAdapter` async trait — interface every backend (S3, IPFS,
//! filesystem, custom) implements to plug into the substrate's
//! blob path.

use std::ops::Range;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::Stream;

use super::blob_ref::{BlobRef, Encoding};
use super::error::BlobError;

/// Stream of byte chunks the substrate consumes from `fetch_stream`.
/// Errors mid-stream surface as `Err(BlobError)`; the consumer
/// stops on the first error and discards any prior chunks (no
/// partial-blob verification).
pub type BlobByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, BlobError>> + Send>>;

/// Operational snapshot returned by [`BlobAdapter::stat`]. Lives at
/// the trait surface (every adapter must answer) but most fields
/// are optional — adapters that can't cheaply observe (S3 / IPFS)
/// fill in only what they know.
#[derive(Clone, Debug, Default)]
pub struct BlobStat {
    /// Total payload size in bytes. Always known when [`BlobAdapter::stat`]
    /// returns `Ok` — adapters that can't determine the size return
    /// [`BlobError::NotFound`] instead.
    pub size: u64,
    /// Number of distinct nodes currently advertising this blob via
    /// `causal:<hex>` capability tags. `0` for adapters that don't
    /// participate in the substrate-side advertisement layer (FS,
    /// S3 adapters); `Some(n)` for `MeshBlobAdapter`. Best-effort —
    /// the count reflects the local node's view of the capability
    /// index at the time of the call.
    pub replicas_observed: u32,
    /// Operator-configured replication factor for this adapter, if
    /// any. `None` for adapters whose durability isn't governed by
    /// the substrate (S3, IPFS — they rely on the backend's own
    /// replication semantics); `Some(n)` for `MeshBlobAdapter`.
    pub replica_target: Option<u8>,
    /// Last wall-clock time (unix milliseconds) the blob was
    /// touched (heartbeat advertisement, fetch, store). `None`
    /// when the adapter doesn't track per-blob last-seen.
    pub last_seen_unix_ms: Option<u64>,
    /// Encoding of the stored content. `Some(Replicated)` for the
    /// v0.2 path; `Some(ReedSolomon { k, m })` reserved for v0.3.
    /// `None` for adapters that don't model encoding (FS, Noop).
    pub encoding: Option<Encoding>,
}

/// Storage backend wrapped by the substrate's blob layer. Each
/// adapter takes a [`BlobRef`]'s URI and serves the bytes it
/// resolves to — the substrate handles hash verification on top.
///
/// `adapter_id` is the registry key (see
/// [`super::registry::BlobAdapterRegistry`]). Distinct identities
/// per adapter so a host can register an S3 backend at
/// `"s3-primary"` and a fallback at `"s3-replica"` without
/// collision.
///
/// The trait is `async` (async-trait crate, mirroring the rest of
/// the cortex / net surface) so adapters can do real I/O without
/// blocking the runtime thread. Sync backends wrap with
/// `tokio::task::block_in_place` or `spawn_blocking`.
#[async_trait]
pub trait BlobAdapter: Send + Sync + 'static {
    /// Stable identifier for this adapter instance. The registry
    /// rejects re-registrations with the same id.
    fn adapter_id(&self) -> &str;

    /// URI schemes this adapter accepts on inbound `BlobRef`s.
    /// The substrate's blob-dispatch layer routes by channel-
    /// configured `blob_adapter_id`; before invoking the adapter
    /// it checks the inbound URI's scheme against this list and
    /// rejects with [`BlobError::UnsupportedScheme`] when the URI
    /// scheme isn't accepted. Default returns an empty slice,
    /// which means "accept anything" — adapters in trusted /
    /// single-tenant deployments may leave this as-is, but
    /// adapters that have authority over a privileged backend
    /// (FS adapter, host-side keys, etc.) should override and
    /// list the schemes they actually serve so a publisher with
    /// append rights cannot dictate arbitrary URIs the adapter
    /// then resolves.
    fn accepted_schemes(&self) -> &[&str] {
        &[]
    }

    /// Persist `bytes` at the URI carried in `blob_ref`. Most
    /// adapters will derive the URI from `blob_ref.hash` (content-
    /// addressing) and ignore the inbound URI; some (e.g.
    /// `FileSystemAdapter`) honor it directly. The hash on
    /// `blob_ref` is the source of truth — the substrate computes
    /// it before calling this method.
    async fn store(&self, blob_ref: &BlobRef, bytes: &[u8]) -> Result<(), BlobError>;

    /// Fetch the full content at `blob_ref.uri`. The substrate
    /// runs [`BlobRef::verify`] on the returned bytes; on a
    /// mismatch the call as a whole fails with
    /// [`BlobError::HashMismatch`].
    async fn fetch(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobError>;

    /// Fetch a byte range. `range.start <= range.end` and both
    /// bounded by `blob_ref.size`; out-of-range queries surface as
    /// [`BlobError::Backend`] from the adapter. The substrate does
    /// NOT verify partial fetches against the full-content hash;
    /// callers using range fetch are accepting that trade-off.
    async fn fetch_range(
        &self,
        blob_ref: &BlobRef,
        range: Range<u64>,
    ) -> Result<Vec<u8>, BlobError>;

    /// Probe for existence without fetching. Adapters that cannot
    /// answer cheaply may emulate by `fetch` + drop; the trait
    /// makes no efficiency promise.
    async fn exists(&self, blob_ref: &BlobRef) -> Result<bool, BlobError>;

    /// Stream the blob content as a sequence of byte chunks.
    /// Default impl routes through [`Self::fetch`] and emits the
    /// whole payload as a single chunk — fine for adapters that
    /// hold blobs in RAM or pull them in one shot anyway (S3
    /// GetObject with no Range, IPFS). Adapters with real
    /// streaming backends (chunked HTTP, mmap'd local files,
    /// range-fetched S3) should override to yield progressively.
    ///
    /// Substrate-side hash verification consumes the stream as it
    /// arrives: hash the chunks incrementally, accumulate into a
    /// buffer (or pipe through to the application), and reject
    /// on completion if the BLAKE3 doesn't match.
    ///
    /// Multi-GB blobs that don't fit in a single buffer must use
    /// this surface; the all-in-memory [`Self::fetch`] is
    /// preserved for short payloads and ergonomic callers.
    async fn fetch_stream(&self, blob_ref: &BlobRef) -> Result<BlobByteStream, BlobError> {
        let bytes = self.fetch(blob_ref).await?;
        let stream = futures::stream::once(async move { Ok(Bytes::from(bytes)) });
        Ok(Box::pin(stream))
    }

    /// Store from a stream of byte chunks. Default impl drains the
    /// stream into a `Vec<u8>` and forwards to [`Self::store`];
    /// adapters with real streaming write paths (S3 multipart
    /// upload, chunked filesystem write) should override.
    ///
    /// The implementation MUST verify the produced bytes hash to
    /// `blob_ref.hash` before considering the store durable. The
    /// substrate's `store` contract requires this; streaming
    /// impls compute the hash incrementally as chunks arrive.
    ///
    /// `size_hint` is the caller's expected total size; adapters
    /// may use it for pre-allocation but must not require it to
    /// match the actual stream length.
    async fn store_stream(
        &self,
        blob_ref: &BlobRef,
        mut stream: BlobByteStream,
        size_hint: Option<u64>,
    ) -> Result<(), BlobError> {
        use futures::StreamExt;
        let mut buf: Vec<u8> = match size_hint {
            Some(n) if (n as usize) <= 16 * 1024 * 1024 => Vec::with_capacity(n as usize),
            _ => Vec::new(),
        };
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk?);
        }
        self.store(blob_ref, &buf).await
    }

    /// Best-effort delete. The substrate calls this on the GC
    /// sweep path (v0.2 [`MeshBlobAdapter`](super::MeshBlobAdapter)); external-storage
    /// adapters (S3 / IPFS) typically defer durability decisions
    /// to the backend's own lifecycle policies and may treat this
    /// as a no-op.
    ///
    /// Default impl: returns `Ok(())` without touching the backend
    /// (no-op delete). Override for adapters that own the blob
    /// lifecycle.
    ///
    /// Manifest-variant semantics — `delete` is **surface-only**:
    /// a [`BlobRef::Manifest`] delete removes the manifest entry
    /// (if any) but does NOT recursively remove its chunks. Chunks
    /// are independently reference-counted at the substrate layer
    /// and delete on their own GC cycle. See
    /// `DATAFORTS_BLOB_STORAGE_PLAN.md` § Q4 for the rationale.
    async fn delete(&self, _blob_ref: &BlobRef) -> Result<(), BlobError> {
        Ok(())
    }

    /// Hint to the adapter that `blob_ref`'s bytes will likely be
    /// fetched soon — kick off any background pre-population
    /// (cross-node replication, prefetch from cold storage,
    /// warm-cache load) without blocking on completion. The
    /// returned `Ok(())` means "the prefetch was initiated", not
    /// "the bytes are now local".
    ///
    /// Default impl: no-op success. Override on adapters with a
    /// meaningful pre-population path —
    /// [`MeshBlobAdapter`](super::MeshBlobAdapter) opens each
    /// constituent chunk channel against the local
    /// [`Redex`](crate::adapter::net::redex::Redex) handle so the
    /// per-chunk replication runtime spawns and begins syncing
    /// from peers carrying the chunk's `causal:<hex>` tag. This is
    /// the wiring greedy uses when its G-1 admit verdict fires
    /// (PR-5i — actual fetch is best-effort, parallel to the
    /// admission decision; greedy doesn't block on the prefetch).
    ///
    /// Errors surface back to the caller as
    /// [`BlobError::Backend`] but are advisory — the calling
    /// runtime typically counts and drops rather than retrying.
    async fn prefetch(&self, _blob_ref: &BlobRef) -> Result<(), BlobError> {
        Ok(())
    }

    /// Return an operational snapshot of the blob. Used by the
    /// `net blob stat` CLI + the metrics exporters; surfaces size,
    /// replica counts (where the adapter knows), encoding, etc.
    ///
    /// Default impl returns the `size` carried on the
    /// [`BlobRef`] with every other field at default — adapters
    /// that participate in the substrate's advertisement layer
    /// (e.g. [`MeshBlobAdapter`](super::MeshBlobAdapter)) should override to fill in
    /// `replicas_observed`, `replica_target`, `encoding`, and
    /// `last_seen_unix_ms`. The size field comes from the
    /// [`BlobRef`] itself, so adapters that don't track per-blob
    /// metadata still answer this method correctly.
    async fn stat(&self, blob_ref: &BlobRef) -> Result<BlobStat, BlobError> {
        Ok(BlobStat {
            size: blob_ref.size(),
            encoding: blob_ref.encoding(),
            ..Default::default()
        })
    }

    /// Enumerate blob chunks the adapter has observed. Powers
    /// the operator-facing "Blob & Artifact Explorer" surface
    /// (`DECK_PLAN.md` § Deferred work § Blob & Artifact
    /// Explorer) — adapters that can cheaply enumerate (Mesh,
    /// fs) override; adapters with prohibitive enumeration
    /// cost (S3 with millions of keys, IPFS) leave the default
    /// "empty" so consumers don't accidentally rack up backend
    /// charges.
    ///
    /// The default returns an empty vec rather than an error
    /// because "this adapter doesn't enumerate" is a normal
    /// answer, not a failure — the BLOBS tab simply shows no
    /// rows for that adapter.
    ///
    /// Granularity is **chunk-level**, not logical-blob-level.
    /// `MeshBlobAdapter` tracks blobs in a refcount table keyed
    /// by content hash: a `BlobRef::Small` corresponds to one
    /// entry, a `BlobRef::Manifest` to N entries (one per
    /// chunk). Reconstructing logical `BlobRef`s would need a
    /// per-store BlobRef index the substrate doesn't carry
    /// today; that's tracked as a follow-on in
    /// `DECK_PLAN.md` § Deferred work § Blob & Artifact
    /// Explorer.
    ///
    /// `opts.prefix_hex` filters by a hex prefix of the
    /// content hash (e.g. `Some("abcd")` returns only chunks
    /// whose hash starts with `0xab 0xcd`). `opts.limit` caps
    /// the result count — adapters may return fewer when
    /// fewer match. Order is unspecified at the trait level
    /// (`MeshBlobAdapter` sorts by `last_seen_unix_ms` desc).
    async fn list(&self, _opts: &BlobListOptions) -> Result<Vec<BlobInventoryEntry>, BlobError> {
        Ok(Vec::new())
    }
}

/// Options for [`BlobAdapter::list`]. Built to grow — additional
/// filters (date range, encoding, refcount band) land here
/// without changing the trait signature.
#[derive(Clone, Debug, Default)]
pub struct BlobListOptions {
    /// Lowercase hex prefix matched against the content hash.
    /// `None` matches every entry. Adapters that can't filter
    /// on the prefix scan all and filter in-memory.
    pub prefix_hex: Option<String>,
    /// Cap on the returned set. `0` (the default for
    /// `BlobListOptions::default()`) is interpreted as "no
    /// caller cap"; consumers reading via the SDK pass a
    /// concrete value (typically 1000–10000) to bound
    /// memory.
    pub limit: usize,
}

/// One row of the operator-facing blob inventory: a content
/// hash the adapter has observed, plus the refcount-table
/// metadata that goes with it. Chunk-level granularity per the
/// note on [`BlobAdapter::list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobInventoryEntry {
    /// `adapter_id()` of the adapter that produced this entry.
    /// Multi-adapter deployments surface this so the operator
    /// can tell which backend holds the chunk; single-adapter
    /// callers can ignore.
    pub adapter_id: String,
    /// 64-character lowercase hex of the blob's BLAKE3 content
    /// hash. The canonical id at this granularity.
    pub hash_hex: String,
    /// Refcount the adapter tracks. `0` means quiescent and on
    /// the GC retention clock; non-zero means at least one
    /// source is holding a live reference.
    pub refcount: u32,
    /// `true` when the operator has explicitly pinned the
    /// entry against GC (operators sometimes pin known-good
    /// chunks during a debug session).
    pub pinned: bool,
    /// First wall-clock unix-ms the adapter observed this
    /// hash (the retention floor measures from here).
    pub first_seen_unix_ms: u64,
    /// Most recent wall-clock unix-ms the adapter observed
    /// this hash (any incr / decr / store).
    pub last_seen_unix_ms: u64,
}
