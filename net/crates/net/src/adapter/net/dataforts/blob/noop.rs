//! `NoopAdapter` — drop-everything backend for tests and
//! local-only deployments that never want to actually persist
//! blob content.
//!
//! `store` succeeds. `fetch` / `fetch_range` / `exists` return
//! `BlobError::NotFound`. Useful when the blob path needs to be
//! wired through plumbing for a build but the workload doesn't
//! actually need out-of-band storage.

use std::ops::Range;

use async_trait::async_trait;

use super::adapter::BlobAdapter;
use super::blob_ref::BlobRef;
use super::error::BlobError;

/// A `BlobAdapter` that discards on `store` and reports
/// `BlobError::NotFound` on every read.
#[derive(Debug, Clone)]
pub struct NoopAdapter {
    id: String,
}

impl NoopAdapter {
    /// Construct a noop adapter with the given registry id.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

impl Default for NoopAdapter {
    fn default() -> Self {
        Self::new("noop")
    }
}

#[async_trait]
impl BlobAdapter for NoopAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    async fn store(&self, _blob_ref: &BlobRef, _bytes: &[u8]) -> Result<(), BlobError> {
        Ok(())
    }

    async fn fetch(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobError> {
        Err(BlobError::NotFound(blob_ref.uri().to_owned()))
    }

    async fn fetch_range(
        &self,
        blob_ref: &BlobRef,
        _range: Range<u64>,
    ) -> Result<Vec<u8>, BlobError> {
        Err(BlobError::NotFound(blob_ref.uri().to_owned()))
    }

    async fn exists(&self, _blob_ref: &BlobRef) -> Result<bool, BlobError> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_ref() -> BlobRef {
        BlobRef::small("noop://x", [0x00; 32], 0)
    }

    #[tokio::test]
    async fn store_succeeds_silently() {
        let a = NoopAdapter::default();
        a.store(&fixture_ref(), b"ignored").await.unwrap();
    }

    #[tokio::test]
    async fn fetch_returns_not_found() {
        let a = NoopAdapter::default();
        let err = a.fetch(&fixture_ref()).await.unwrap_err();
        assert!(matches!(err, BlobError::NotFound(_)));
    }

    #[tokio::test]
    async fn exists_returns_false() {
        let a = NoopAdapter::default();
        assert!(!a.exists(&fixture_ref()).await.unwrap());
    }

    #[test]
    fn id_round_trips_through_constructor() {
        let a = NoopAdapter::new("my-noop");
        assert_eq!(a.adapter_id(), "my-noop");
    }
}
