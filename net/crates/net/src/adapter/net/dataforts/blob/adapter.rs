//! `BlobAdapter` async trait — interface every backend (S3, IPFS,
//! filesystem, custom) implements to plug into the substrate's
//! blob path.

use std::ops::Range;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::Stream;

use super::blob_ref::BlobRef;
use super::error::BlobError;

/// Stream of byte chunks the substrate consumes from `fetch_stream`.
/// Errors mid-stream surface as `Err(BlobError)`; the consumer
/// stops on the first error and discards any prior chunks (no
/// partial-blob verification).
pub type BlobByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, BlobError>> + Send>>;

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
}
