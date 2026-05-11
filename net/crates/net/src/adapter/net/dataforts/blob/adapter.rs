//! `BlobAdapter` async trait — interface every backend (S3, IPFS,
//! filesystem, custom) implements to plug into the substrate's
//! blob path.

use std::ops::Range;

use async_trait::async_trait;

use super::error::BlobError;
use super::blob_ref::BlobRef;

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
}
