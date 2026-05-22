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
/// Contract steps:
/// 1. `exists` reports `false` for an unwritten blob.
/// 2. `store` succeeds.
/// 3. `exists` reports `true` after store.
/// 4. `fetch` returns the stored bytes and they pass
///    [`BlobRef::verify`].
/// 5. `fetch_range` returns the right slice for a mid-blob range.
/// 6. `fetch_range` returns an empty `Vec` for an empty range.
/// 7. `store` is idempotent — repeating with the same hash + bytes
///    succeeds and leaves the content unchanged.
/// 8. `store` rejects mismatched bytes (claimed hash != BLAKE3 of
///    the bytes) with [`BlobError::HashMismatch`]. This is the
///    cache-poisoning defense the original review identified;
///    every adapter must hash inside store.
/// 9. `fetch_range` past the blob size returns an error (variant
///    is adapter-specific; we only require Err).
/// 10. Cross-blob isolation — fetching one hash never returns
///     another stored blob's bytes.
/// 11. `fetch` on a randomized missing-blob hash returns
///     [`BlobError::NotFound`]. The hash is per-run so an
///     adversarial adapter that hardcodes a sentinel can't pass.
pub async fn run_conformance_suite<A: BlobAdapter + ?Sized>(
    adapter: &A,
) -> Result<(), &'static str> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SUITE_NONCE: AtomicU64 = AtomicU64::new(1);

    let run_nonce = SUITE_NONCE.fetch_add(1, Ordering::Relaxed);
    let payload: Vec<u8> = format!(
        "conformance-fixture-payload-{}-{}",
        std::process::id(),
        run_nonce
    )
    .into_bytes();
    let hash: [u8; 32] = blake3::hash(&payload).into();
    let blob = BlobRef::small(
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
        .store(&blob, &payload)
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
    if slice.as_ref() != &payload[5..10] {
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

    // Idempotent re-store — same hash + bytes; content remains.
    adapter
        .store(&blob, &payload)
        .await
        .map_err(|_| "idempotent re-store failed")?;
    let refetched = adapter
        .fetch(&blob)
        .await
        .map_err(|_| "fetch after re-store failed")?;
    if refetched != payload {
        return Err("idempotent re-store changed content");
    }

    // Hash-mismatch rejection — claim hash of `payload`, send bytes
    // of `tampered`. Every adapter must verify and refuse.
    let tampered: Vec<u8> = format!("tampered-{}", run_nonce).into_bytes();
    let tampered_hash: [u8; 32] = blake3::hash(&tampered).into();
    if !tampered.is_empty() && Some(&tampered_hash) != blob.small_hash() {
        match adapter.store(&blob, &tampered).await {
            Err(BlobError::HashMismatch { .. }) => {}
            Ok(()) => {
                return Err("store accepted bytes that don't hash to the claimed BlobRef.hash")
            }
            Err(_) => return Err("store rejected mismatched bytes with the wrong error variant"),
        }
    }

    // fetch_range past end must error rather than silently truncate.
    let past_end = blob.size() + 1;
    if adapter
        .fetch_range(&blob, blob.size()..past_end)
        .await
        .is_ok()
    {
        return Err("fetch_range past end returned Ok");
    }

    // Cross-blob isolation — a second blob's content must not
    // bleed into the first blob's fetch.
    let second_payload: Vec<u8> =
        format!("second-payload-{}-{}", std::process::id(), run_nonce).into_bytes();
    let second_hash: [u8; 32] = blake3::hash(&second_payload).into();
    let second_blob = BlobRef::small(
        format!("conformance://{}/second", adapter.adapter_id()),
        second_hash,
        second_payload.len() as u64,
    );
    adapter
        .store(&second_blob, &second_payload)
        .await
        .map_err(|_| "second store failed")?;
    let first_again = adapter
        .fetch(&blob)
        .await
        .map_err(|_| "fetch first after second store failed")?;
    if first_again != payload {
        return Err("second blob's store corrupted first blob's content");
    }

    // fetch_stream returns the same bytes as fetch — adapters
    // that override stream must keep parity with the all-in-memory
    // path. Bytes accumulated from the stream must verify against
    // the BlobRef's hash.
    {
        use futures::StreamExt;
        let mut stream = adapter
            .fetch_stream(&blob)
            .await
            .map_err(|_| "fetch_stream failed")?;
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "fetch_stream chunk error")?;
            buf.extend_from_slice(&chunk);
        }
        if buf != payload {
            return Err("fetch_stream bytes != stored bytes");
        }
        blob.verify(&buf)
            .map_err(|_| "BlobRef::verify on fetch_stream bytes failed")?;
    }

    // Missing blob — random hash per run so an adversarial adapter
    // can't hardcode a sentinel response.
    let mut ghost_hash = [0u8; 32];
    let nonce_bytes = run_nonce.to_le_bytes();
    ghost_hash[..8].copy_from_slice(&nonce_bytes);
    ghost_hash[8..16].copy_from_slice(&nonce_bytes);
    // Top half: process id and a fixed marker so adapters can't
    // detect the suite by sentinel matching.
    ghost_hash[16..24].copy_from_slice(&(std::process::id() as u64).to_le_bytes());
    ghost_hash[24..32].copy_from_slice(b"NO-GHOST");
    let ghost = BlobRef::small(
        format!("conformance://{}/ghost-{}", adapter.adapter_id(), run_nonce),
        ghost_hash,
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
