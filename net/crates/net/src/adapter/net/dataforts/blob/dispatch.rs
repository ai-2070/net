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
/// a `Vec<u8>` copy; blob-ref payloads validate the URI scheme
/// against the adapter's accepted-schemes list, fetch via
/// `adapter`, verify against the embedded BLAKE3 hash, and return
/// the verified bytes.
///
/// Scheme validation closes the publisher-controls-adapter-input
/// attack surface: a publisher with append rights on a channel
/// configured to use a privileged adapter (e.g. an FS adapter
/// with host-side authority) could otherwise stamp arbitrary
/// `s3://attacker/key` URIs that the FS adapter would still try
/// to resolve. The adapter's [`BlobAdapter::accepted_schemes`]
/// override drives the gate — empty default means "accept any."
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
            let accepted = adapter.accepted_schemes();
            if !accepted.is_empty() {
                let scheme = uri_scheme(blob.uri());
                if !accepted.contains(&scheme) {
                    return Err(BlobError::UnsupportedScheme(blob.uri().to_owned()));
                }
            }
            let fetched = adapter.fetch(&blob).await?;
            // Verification posture is different for the two BlobRef
            // shapes:
            //   * `Small` — single top-level BLAKE3 hash. The
            //     substrate runs `BlobRef::verify` here, independent
            //     of the adapter, so an adversarial / buggy adapter
            //     can't fake-verify by returning bytes that match a
            //     hash it computed itself.
            //   * `Manifest` — no top-level hash. The substrate
            //     runs the chunk-by-chunk verification here too:
            //     slice `fetched` at `chunks[i].size` boundaries,
            //     hash each region, compare against the manifest's
            //     `chunks[i].hash`. This used to be left to the
            //     adapter (e.g. `MeshBlobAdapter::fetch_chunk`
            //     internally), but `resolve_payload` accepts any
            //     `BlobAdapter`-impl and adapters that don't verify
            //     chunk-by-chunk (or are adversarial) could
            //     otherwise return tampered bytes here. Doing the
            //     check at the dispatch layer keeps the substrate-
            //     side guarantee uniform across both shapes.
            if !blob.is_chunked() {
                blob.verify(&fetched)?;
            } else {
                verify_manifest_chunks(&blob, &fetched)?;
            }
            Ok(fetched)
        }
    }
}

/// Verify a `Manifest`-shape `BlobRef` against the reassembled
/// `fetched` byte stream. Walks the manifest's `chunks` list,
/// slices `fetched` at the recorded chunk sizes, hashes each
/// slice with BLAKE3, and compares against the recorded
/// `chunks[i].hash`. Surface a typed error on any mismatch so the
/// caller surfaces a verification failure instead of returning
/// tampered bytes.
///
/// Independent of any adapter-side verification — adversarial or
/// buggy adapters that return manipulated bytes here will fail
/// the substrate-side check.
fn verify_manifest_chunks(
    blob: &super::blob_ref::BlobRef,
    fetched: &[u8],
) -> Result<(), BlobError> {
    use super::blob_ref::BlobRef;
    let chunks = match blob {
        BlobRef::Manifest { chunks, .. } => chunks,
        // The caller only invokes this on `is_chunked() == true`,
        // so this branch is unreachable in production. Guard
        // anyway for forward-compat with new BlobRef variants.
        BlobRef::Small { .. } => return Ok(()),
    };
    let total: u64 = chunks.iter().map(|c| c.size as u64).sum();
    if total != fetched.len() as u64 {
        return Err(BlobError::Backend(format!(
            "manifest reassembled length {} != sum of chunk sizes {}",
            fetched.len(),
            total
        )));
    }
    let mut offset: usize = 0;
    for chunk in chunks.iter() {
        let end = offset + chunk.size as usize;
        let region = &fetched[offset..end];
        let computed: [u8; 32] = blake3::hash(region).into();
        if computed != chunk.hash {
            return Err(BlobError::HashMismatch {
                expected: chunk.hash,
                actual: computed,
            });
        }
        offset = end;
    }
    Ok(())
}

/// Extract the URI scheme (everything before the first `:`), or
/// the empty string if no scheme is present.
fn uri_scheme(uri: &str) -> &str {
    match uri.find(':') {
        Some(i) => &uri[..i],
        None => "",
    }
}

/// Write `bytes` to `adapter` and return the encoded [`BlobRef`]
/// the caller should publish (via `RedexFile::append`,
/// `MeshNode::publish`, or any path that takes raw event-payload
/// bytes). The substrate computes the BLAKE3 hash, so the
/// returned ref is guaranteed to verify against the stored
/// content when later fetched through [`resolve_payload`].
///
/// The returned `Vec<u8>` is the encoded form, ready to use as an
/// event payload. Callers wanting the structured `BlobRef` can use
/// [`publish_blob_ref`] instead.
pub async fn publish_blob<A: BlobAdapter + ?Sized>(
    adapter: &A,
    uri: impl Into<String>,
    bytes: &[u8],
) -> Result<Vec<u8>, BlobError> {
    let blob = publish_blob_ref(adapter, uri, bytes).await?;
    Ok(blob.encode())
}

/// Same as [`publish_blob`], but returns the structured
/// [`BlobRef`] instead of the encoded form. Useful when the caller
/// wants to surface the URI / hash / size separately (e.g. for
/// telemetry or a side-channel index).
pub async fn publish_blob_ref<A: BlobAdapter + ?Sized>(
    adapter: &A,
    uri: impl Into<String>,
    bytes: &[u8],
) -> Result<BlobRef, BlobError> {
    let hash: [u8; 32] = blake3::hash(bytes).into();
    let blob = BlobRef::small(uri, hash, bytes.len() as u64);
    adapter.store(&blob, bytes).await?;
    Ok(blob)
}

#[cfg(test)]
mod tests {
    use super::super::fs::FileSystemAdapter;
    use super::super::noop::NoopAdapter;
    use super::*;

    fn fixture_blob_ref(payload: &[u8]) -> BlobRef {
        BlobRef::small(
            "file:///dispatch",
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
    async fn publish_blob_round_trips_through_resolve_payload() {
        let root = std::env::temp_dir().join(format!(
            "net-blob-publish-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let adapter = FileSystemAdapter::new("publish-test", &root);
        let payload = b"write side equivalent of resolve_payload";

        // publish_blob returns the encoded BlobRef as bytes.
        let encoded = publish_blob(&adapter, "file:///published", payload)
            .await
            .unwrap();
        // First four bytes are the BlobRef magic.
        assert_eq!(
            &encoded[..4],
            &crate::adapter::net::dataforts::blob::BLOB_REF_MAGIC,
        );

        // resolve_payload turns the encoded form back into the
        // original bytes via fetch + verify.
        let resolved = resolve_payload(&encoded, &adapter).await.unwrap();
        assert_eq!(resolved, payload);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn publish_blob_ref_returns_structured_ref() {
        let root = std::env::temp_dir().join(format!(
            "net-blob-publish-ref-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let adapter = FileSystemAdapter::new("publish-ref", &root);
        let payload = b"explicit ref shape";

        let blob = publish_blob_ref(&adapter, "file:///structured", payload)
            .await
            .unwrap();
        // Hash is BLAKE3 of the payload.
        let expected: [u8; 32] = blake3::hash(payload).into();
        assert_eq!(blob.small_hash(), Some(&expected));
        assert_eq!(blob.size(), payload.len() as u64);
        assert_eq!(blob.uri(), "file:///structured");

        // Stored content is fetchable + verifies.
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched, payload);
        blob.verify(&fetched).unwrap();

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `BlobRef::verify` hard-errors on the Manifest arm because
    /// Manifest has no top-level hash; chunk hashes are verified
    /// per-chunk inside the adapter (`MeshBlobAdapter::fetch_chunk`).
    /// resolve_payload must skip the top-level verify for Manifest,
    /// otherwise every chunked payload (anything over 4 MiB) is
    /// un-fetchable through the documented helper.
    #[tokio::test]
    async fn resolve_passes_chunked_manifest_through_without_top_level_verify() {
        use super::super::adapter::BlobAdapter;
        use super::super::blob_ref::{ChunkRef, Encoding, BLOB_CHUNK_SIZE_BYTES};

        // Adapter that returns the same payload for any Manifest
        // fetch. resolve_payload must NOT try to BlobRef::verify
        // those bytes against the (non-existent) top-level hash.
        #[derive(Debug)]
        struct StubManifestAdapter(Vec<u8>);
        #[async_trait::async_trait]
        impl BlobAdapter for StubManifestAdapter {
            fn adapter_id(&self) -> &str {
                "stub-manifest"
            }
            async fn store(&self, _: &BlobRef, _: &[u8]) -> Result<(), BlobError> {
                Ok(())
            }
            async fn fetch(&self, _: &BlobRef) -> Result<Vec<u8>, BlobError> {
                Ok(self.0.clone())
            }
            async fn fetch_range(
                &self,
                _: &BlobRef,
                range: std::ops::Range<u64>,
            ) -> Result<Vec<u8>, BlobError> {
                Ok(self.0[range.start as usize..range.end as usize].to_vec())
            }
            async fn exists(&self, _: &BlobRef) -> Result<bool, BlobError> {
                Ok(true)
            }
        }

        let payload = vec![0x5A; (BLOB_CHUNK_SIZE_BYTES as usize) + 16];
        let chunk_1 = vec![0x5A; BLOB_CHUNK_SIZE_BYTES as usize];
        let chunk_2 = vec![0x5A; 16];
        let chunks = vec![
            ChunkRef {
                hash: blake3::hash(&chunk_1).into(),
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
            ChunkRef {
                hash: blake3::hash(&chunk_2).into(),
                size: 16,
            },
        ];
        let blob = BlobRef::manifest("mesh://chunked-resolve", Encoding::Replicated, chunks)
            .expect("manifest construct");
        let encoded = blob.encode();
        let adapter = StubManifestAdapter(payload.clone());
        let resolved = resolve_payload(&encoded, &adapter)
            .await
            .expect("resolve must accept Manifest without top-level verify");
        assert_eq!(resolved, payload);
    }

    /// Review P1 regression: a chunked Manifest fetched via
    /// `resolve_payload` must verify each chunk against the
    /// manifest's recorded hashes — adversarial adapters that
    /// return tampered chunk bytes must fail with `HashMismatch`
    /// at the dispatch layer (independent of any adapter-side
    /// verification, which third-party impls may or may not do).
    #[tokio::test]
    async fn resolve_rejects_chunked_manifest_with_tampered_chunk_bytes() {
        use super::super::adapter::BlobAdapter;
        use super::super::blob_ref::{ChunkRef, Encoding, BLOB_CHUNK_SIZE_BYTES};

        // Stub that returns the legitimate first chunk but flips
        // the second chunk's payload — simulates an adversarial
        // / buggy adapter.
        #[derive(Debug)]
        struct TamperingAdapter {
            payload: Vec<u8>,
        }
        #[async_trait::async_trait]
        impl BlobAdapter for TamperingAdapter {
            fn adapter_id(&self) -> &str {
                "tampering"
            }
            async fn store(&self, _: &BlobRef, _: &[u8]) -> Result<(), BlobError> {
                Ok(())
            }
            async fn fetch(&self, _: &BlobRef) -> Result<Vec<u8>, BlobError> {
                Ok(self.payload.clone())
            }
            async fn fetch_range(
                &self,
                _: &BlobRef,
                range: std::ops::Range<u64>,
            ) -> Result<Vec<u8>, BlobError> {
                Ok(self.payload[range.start as usize..range.end as usize].to_vec())
            }
            async fn exists(&self, _: &BlobRef) -> Result<bool, BlobError> {
                Ok(true)
            }
        }

        // Legitimate manifest: first chunk all 0xAA, second chunk
        // all 0xBB. Hashes are recorded against the legitimate
        // bytes.
        let legit_chunk_1 = vec![0xAA; BLOB_CHUNK_SIZE_BYTES as usize];
        let legit_chunk_2 = vec![0xBB; 16];
        let chunks = vec![
            ChunkRef {
                hash: blake3::hash(&legit_chunk_1).into(),
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
            ChunkRef {
                hash: blake3::hash(&legit_chunk_2).into(),
                size: 16,
            },
        ];
        let blob = BlobRef::manifest("mesh://tampered", Encoding::Replicated, chunks)
            .expect("manifest construct");
        let encoded = blob.encode();

        // Tampered payload: first chunk matches the manifest's
        // recorded hash; second chunk is flipped to all 0xCC.
        let mut tampered = legit_chunk_1.clone();
        tampered.extend(vec![0xCC; 16]);
        let adapter = TamperingAdapter { payload: tampered };
        let err = resolve_payload(&encoded, &adapter)
            .await
            .expect_err("tampered chunk must fail verification");
        assert!(
            matches!(err, BlobError::HashMismatch { .. }),
            "expected HashMismatch on tampered chunk 2, got {:?}",
            err
        );
    }

    /// Companion to the legitimate-path test above: a Manifest
    /// fetched via `resolve_payload` whose adapter returns the
    /// genuine bytes must still succeed (the chunk-by-chunk
    /// verifier accepts every matching chunk hash).
    #[tokio::test]
    async fn resolve_accepts_chunked_manifest_with_matching_chunk_bytes() {
        use super::super::adapter::BlobAdapter;
        use super::super::blob_ref::{ChunkRef, Encoding, BLOB_CHUNK_SIZE_BYTES};

        #[derive(Debug)]
        struct LegitAdapter(Vec<u8>);
        #[async_trait::async_trait]
        impl BlobAdapter for LegitAdapter {
            fn adapter_id(&self) -> &str {
                "legit"
            }
            async fn store(&self, _: &BlobRef, _: &[u8]) -> Result<(), BlobError> {
                Ok(())
            }
            async fn fetch(&self, _: &BlobRef) -> Result<Vec<u8>, BlobError> {
                Ok(self.0.clone())
            }
            async fn fetch_range(
                &self,
                _: &BlobRef,
                range: std::ops::Range<u64>,
            ) -> Result<Vec<u8>, BlobError> {
                Ok(self.0[range.start as usize..range.end as usize].to_vec())
            }
            async fn exists(&self, _: &BlobRef) -> Result<bool, BlobError> {
                Ok(true)
            }
        }

        let chunk_1 = vec![0x11; BLOB_CHUNK_SIZE_BYTES as usize];
        let chunk_2 = vec![0x22; 32];
        let mut full = chunk_1.clone();
        full.extend(&chunk_2);
        let chunks = vec![
            ChunkRef {
                hash: blake3::hash(&chunk_1).into(),
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
            ChunkRef {
                hash: blake3::hash(&chunk_2).into(),
                size: 32,
            },
        ];
        let blob = BlobRef::manifest("mesh://legit", Encoding::Replicated, chunks)
            .expect("manifest construct");
        let encoded = blob.encode();
        let adapter = LegitAdapter(full.clone());
        let resolved = resolve_payload(&encoded, &adapter)
            .await
            .expect("legitimate manifest must verify");
        assert_eq!(resolved, full);
    }

    #[tokio::test]
    async fn resolve_rejects_uri_with_unaccepted_scheme() {
        // FileSystemAdapter only accepts `file:` URIs. An event
        // payload whose BlobRef carries `s3://attacker/key` must
        // reject with UnsupportedScheme before the adapter is
        // asked to fetch anything.
        let root = std::env::temp_dir().join(format!(
            "net-blob-scheme-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let adapter = FileSystemAdapter::new("scheme-test", &root);
        let payload = b"unused";
        let blob = BlobRef::small(
            "s3://attacker/key",
            blake3::hash(payload).into(),
            payload.len() as u64,
        );
        let encoded = blob.encode();
        let err = resolve_payload(&encoded, &adapter).await.unwrap_err();
        assert!(matches!(err, BlobError::UnsupportedScheme(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn resolve_rejects_blob_with_corrupted_content() {
        // Pin that the substrate-side verify defends against an
        // adversarial adapter that returns bytes whose hash
        // doesn't match the advertised BlobRef. Built on a stub
        // adapter that doesn't verify on store (the production FS
        // adapter does, so we can't use it to forge mismatched
        // content).
        use async_trait::async_trait;
        use std::ops::Range;

        struct AdversarialAdapter {
            id: String,
            bytes: Vec<u8>,
        }
        #[async_trait]
        impl BlobAdapter for AdversarialAdapter {
            fn adapter_id(&self) -> &str {
                &self.id
            }
            async fn store(&self, _blob_ref: &BlobRef, _bytes: &[u8]) -> Result<(), BlobError> {
                Ok(())
            }
            async fn fetch(&self, _blob_ref: &BlobRef) -> Result<Vec<u8>, BlobError> {
                Ok(self.bytes.clone())
            }
            async fn fetch_range(
                &self,
                _blob_ref: &BlobRef,
                _range: Range<u64>,
            ) -> Result<Vec<u8>, BlobError> {
                Ok(self.bytes.clone())
            }
            async fn exists(&self, _blob_ref: &BlobRef) -> Result<bool, BlobError> {
                Ok(true)
            }
        }

        let advertised = b"the truth";
        let actual: &[u8] = b"a different lie";
        let blob = BlobRef::small(
            "test://tamper",
            blake3::hash(advertised).into(),
            advertised.len() as u64,
        );
        let adapter = AdversarialAdapter {
            id: "tamper".into(),
            bytes: actual.to_vec(),
        };
        let encoded = blob.encode();
        let err = resolve_payload(&encoded, &adapter).await.unwrap_err();
        assert!(matches!(err, BlobError::HashMismatch { .. }));
    }
}
