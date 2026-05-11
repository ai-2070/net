//! Read-path dispatch helpers — bridge raw event-payload bytes to
//! the inline / blob-ref distinction without requiring substrate
//! changes to the read APIs.
//!
//! Two shapes per `DATAFORTS_PLAN.md` § Phase 3 work-item 8:
//!
//! - [`classify_payload`] — peek the discriminator byte; return
//!   either the inline bytes view or a decoded [`BlobRef`]. Cheap;
//!   no async work, no adapter lookup.
//! - [`resolve_payload`] — transparent fetch path. Returns the
//!   resolved bytes for both inline payloads (passthrough) and
//!   blob-ref payloads (adapter fetch + hash verify). Callers that
//!   don't want to know which is which use this and treat every
//!   event payload uniformly.
//!
//! Routing by `adapter_id` is the caller's job — the plan's locked
//! decision picks per channel via `RedexFileConfig::blob_adapter_id`
//! (additive substrate change not yet shipped). For now,
//! [`resolve_payload`] takes the chosen adapter directly so callers
//! can build their own routing on top.

use super::adapter::BlobAdapter;
use super::blob_ref::BlobRef;
use super::error::BlobError;

/// Classification of an event payload's wire shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventPayload<'a> {
    /// Plain inline payload — the bytes are the event content
    /// verbatim. Borrowed from the caller's buffer; the caller
    /// owns the lifetime.
    Inline(&'a [u8]),
    /// Out-of-band content addressed by a [`BlobRef`]. The caller
    /// resolves via a [`BlobAdapter`]; the substrate's own
    /// verification path runs as part of [`resolve_payload`].
    Blob(BlobRef),
}

/// Peek a payload's discriminator and produce either the inline
/// borrow or the decoded blob reference. No I/O. No allocation
/// for the inline path; one allocation (the decoded URI string)
/// for the blob path.
pub fn classify_payload(bytes: &[u8]) -> Result<EventPayload<'_>, BlobError> {
    match BlobRef::decode(bytes)? {
        Some(blob) => Ok(EventPayload::Blob(blob)),
        None => Ok(EventPayload::Inline(bytes)),
    }
}

/// Resolve a payload to its content bytes. Inline payloads return
/// a `Vec<u8>` copy; blob-ref payloads fetch via `adapter`, verify
/// against the embedded BLAKE3 hash, and return the verified bytes.
///
/// Hash verification runs inside this function rather than inside
/// the adapter so an adversarial adapter cannot fake-verify by
/// returning bytes that match a hash it computed itself.
pub async fn resolve_payload<A: BlobAdapter + ?Sized>(
    bytes: &[u8],
    adapter: &A,
) -> Result<Vec<u8>, BlobError> {
    match classify_payload(bytes)? {
        EventPayload::Inline(b) => Ok(b.to_vec()),
        EventPayload::Blob(blob) => {
            let fetched = adapter.fetch(&blob).await?;
            blob.verify(&fetched)?;
            Ok(fetched)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::fs::FileSystemAdapter;
    use super::super::noop::NoopAdapter;

    fn fixture_blob_ref(payload: &[u8]) -> BlobRef {
        BlobRef::new(
            "test://dispatch",
            blake3::hash(payload).into(),
            payload.len() as u64,
        )
    }

    #[test]
    fn classify_inline_when_first_byte_is_not_discriminator() {
        let bytes = b"plain payload";
        match classify_payload(bytes).unwrap() {
            EventPayload::Inline(b) => assert_eq!(b, bytes),
            other => panic!("expected Inline, got {:?}", other),
        }
    }

    #[test]
    fn classify_blob_when_first_byte_is_discriminator() {
        let payload = b"out of band";
        let blob = fixture_blob_ref(payload);
        let encoded = blob.encode();
        match classify_payload(&encoded).unwrap() {
            EventPayload::Blob(decoded) => assert_eq!(decoded, blob),
            other => panic!("expected Blob, got {:?}", other),
        }
    }

    #[test]
    fn classify_empty_payload_is_inline() {
        // Empty event payloads exist (heartbeats, ack frames).
        // First-byte peek returns None, so classify reports Inline.
        let bytes: &[u8] = &[];
        match classify_payload(bytes).unwrap() {
            EventPayload::Inline(b) => assert!(b.is_empty()),
            other => panic!("expected empty Inline, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn resolve_passes_inline_through() {
        let adapter = NoopAdapter::default();
        let bytes = b"inline goes straight through";
        let resolved = resolve_payload(bytes, &adapter).await.unwrap();
        assert_eq!(resolved, bytes);
    }

    #[tokio::test]
    async fn resolve_fetches_and_verifies_blob() {
        let root = std::env::temp_dir().join(format!(
            "net-blob-resolve-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let adapter = FileSystemAdapter::new("resolve-test", &root);
        let payload = b"this content lives out of band";
        let blob = fixture_blob_ref(payload);
        adapter.store(&blob, payload).await.unwrap();

        let encoded = blob.encode();
        let resolved = resolve_payload(&encoded, &adapter).await.unwrap();
        assert_eq!(resolved, payload);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn resolve_rejects_blob_with_corrupted_content() {
        // Build a BlobRef whose hash claims one payload, but the
        // adapter serves a different one. Verification inside
        // resolve_payload must fail — pins that the substrate-side
        // check defends against an adversarial adapter.
        let root = std::env::temp_dir().join(format!(
            "net-blob-tamper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let adapter = FileSystemAdapter::new("tamper-test", &root);
        let advertised = b"the truth";
        let actual = b"a different lie";
        let blob = BlobRef::new(
            "test://tamper",
            blake3::hash(advertised).into(),
            advertised.len() as u64,
        );
        // Store mismatching bytes under the BlobRef's hash-derived
        // path. We can't trick the FS adapter directly, so use
        // store() against a BlobRef built from `actual`, then call
        // resolve with an encoded version of the `advertised`
        // BlobRef pointing at the wrong hash slot.
        let actual_blob = BlobRef::new(
            "test://tamper",
            blake3::hash(actual).into(),
            actual.len() as u64,
        );
        adapter.store(&actual_blob, actual).await.unwrap();

        // Construct an encoded BlobRef that LIES about its hash:
        // hash points at `actual_blob`'s storage slot but claims
        // the `advertised` hash. The FS adapter resolves by hash
        // → returns `actual`, but verify uses the advertised hash
        // → mismatch.
        let lying = BlobRef {
            version: 0x01,
            uri: "test://tamper".into(),
            hash: actual_blob.hash, // path to existing storage
            size: actual.len() as u64,
        };
        // First sanity-pin: lying actually fetches `actual`.
        let raw = adapter.fetch(&lying).await.unwrap();
        assert_eq!(raw, actual);

        // Now build the lying BlobRef but with the advertised
        // (mismatched) hash; the lie surfaces when verify runs.
        let liar_encoded = {
            let mut blob = blob.clone();
            // Keep advertised hash, but point uri at the path the
            // adapter actually resolves — already done above.
            blob.uri = "test://tamper".into();
            // The FS adapter ignores URI for storage and uses hash
            // alone, so the FS adapter will look for blob.hash on
            // disk. To force the FS adapter to serve `actual`,
            // store actual under blob.hash too:
            adapter.store(&blob, actual).await.unwrap();
            blob.encode()
        };

        let err = resolve_payload(&liar_encoded, &adapter).await.unwrap_err();
        assert!(matches!(err, BlobError::HashMismatch { .. }));

        let _ = std::fs::remove_dir_all(&root);
    }
}
