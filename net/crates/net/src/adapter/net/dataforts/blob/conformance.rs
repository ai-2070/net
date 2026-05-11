//! `run_conformance_suite` — bundles the contract every
//! [`super::BlobAdapter`] impl must honor into one reusable async
//! fixture.
//!
//! New adapter implementations (S3, IPFS, custom backends) ship
//! their conformance test by calling this from a `#[tokio::test]`
//! against an adapter they constructed themselves. Centralising
//! the suite means a behavior change in the trait contract lands
//! in one place + every adapter picks it up; per-adapter tests
//! stay free to add backend-specific cases on top.

use super::adapter::BlobAdapter;
use super::blob_ref::BlobRef;
use super::error::BlobError;

/// Run the standard `BlobAdapter` contract suite against `adapter`.
/// On any failure, returns the failing assertion as a static
/// `&'static str` so the caller can surface it cleanly.
///
/// Steps:
/// 1. `exists` reports `false` for an unwritten blob.
/// 2. `store` succeeds.
/// 3. `exists` reports `true` after store.
/// 4. `fetch` returns the stored bytes and they pass
///    [`BlobRef::verify`].
/// 5. `fetch_range` returns the right slice for a mid-blob range.
/// 6. `fetch_range` returns an empty `Vec` for an empty range.
/// 7. `fetch` on a fresh `BlobRef` whose hash points nowhere
///    returns [`BlobError::NotFound`].
pub async fn run_conformance_suite<A: BlobAdapter + ?Sized>(
    adapter: &A,
) -> Result<(), &'static str> {
    let payload: &[u8] = b"conformance-fixture-payload-0123456789";
    let hash: [u8; 32] = blake3::hash(payload).into();
    let blob = BlobRef::new(
        format!("conformance://{}/payload", adapter.adapter_id()),
        hash,
        payload.len() as u64,
    );

    if adapter
        .exists(&blob)
        .await
        .map_err(|_| "exists pre-store")?
    {
        return Err("exists returned true before store");
    }
    adapter
        .store(&blob, payload)
        .await
        .map_err(|_| "store failed")?;
    if !adapter
        .exists(&blob)
        .await
        .map_err(|_| "exists post-store")?
    {
        return Err("exists returned false after store");
    }
    let fetched = adapter
        .fetch(&blob)
        .await
        .map_err(|_| "fetch after store failed")?;
    if fetched != payload {
        return Err("fetched bytes != stored bytes");
    }
    blob.verify(&fetched)
        .map_err(|_| "BlobRef::verify on fetched bytes failed")?;

    // Mid-blob slice.
    let slice = adapter
        .fetch_range(&blob, 5..10)
        .await
        .map_err(|_| "fetch_range mid failed")?;
    if slice != &payload[5..10] {
        return Err("fetch_range mid returned wrong slice");
    }

    // Empty range.
    let empty = adapter
        .fetch_range(&blob, 3..3)
        .await
        .map_err(|_| "fetch_range empty failed")?;
    if !empty.is_empty() {
        return Err("fetch_range empty returned non-empty result");
    }

    // Missing blob.
    let ghost = BlobRef::new(
        format!("conformance://{}/ghost", adapter.adapter_id()),
        [0xFE; 32],
        0,
    );
    match adapter.fetch(&ghost).await {
        Err(BlobError::NotFound(_)) => {}
        Ok(_) => return Err("fetch of missing blob returned Ok"),
        Err(_) => return Err("fetch of missing blob returned wrong error variant"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::fs::FileSystemAdapter;
    use super::super::noop::NoopAdapter;
    use super::*;

    #[tokio::test]
    async fn fs_adapter_passes_full_conformance_suite() {
        let root = std::env::temp_dir().join(format!(
            "net-blob-conformance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let adapter = FileSystemAdapter::new("conformance-fs", &root);
        run_conformance_suite(&adapter).await.unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn noop_adapter_fails_at_store_visibility() {
        // The NoopAdapter accepts store but never persists, so
        // exists/fetch return their not-found shapes. The
        // conformance suite catches this — locking that the
        // suite IS strict enough to reject a real-world stub.
        let adapter = NoopAdapter::new("conformance-noop");
        let err = run_conformance_suite(&adapter).await.unwrap_err();
        assert_eq!(err, "exists returned false after store");
    }
}
