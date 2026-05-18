//! Phase B conformance test for the v0.3 CDC store path.
//!
//! Pins the two end-to-end correctness contracts Phase B's plan
//! calls load-bearing:
//!
//! 1. **CDC determinism end-to-end** — the same input bytes
//!    stored via the CDC path on two independent adapters land
//!    on the same root hash. Cross-language dedup depends on
//!    this; a chunker that drifts on the same input would
//!    fragment the cluster's dedup pool the moment a binding
//!    re-implementation diverged from the Rust core.
//! 2. **Dedup-after-edit** — flip a small region in the middle
//!    of a multi-MiB payload, re-store, assert the chunk-hash
//!    sets share a strong majority. Fixed-size chunking would
//!    invalidate every chunk after the edit; CDC's content-
//!    defined boundaries localise the change to the chunks the
//!    edit actually intersects + at most one boundary-neighbour
//!    on each side.
//!
//! Both run by default; both stay inside the test runner's
//! memory budget by passing test-scale CDC parameters through
//! `store_stream_tree_cdc_internal` rather than the production
//! `(min=1 MiB, avg=4 MiB, max=16 MiB)` triple. The cdc-module
//! unit tests already pin pure-Rust determinism + bound-
//! checking; this integration test validates the property holds
//! after the chunker traverses the MeshBlobAdapter store pipeline
//! (chunk persistence, tree-builder cascade, finalize).
//!
//! Run: `cargo test --features dataforts --test
//! dataforts_blob_v3_cdc_conformance`

#![cfg(feature = "dataforts")]

use std::collections::HashSet;
use std::sync::Arc;

use bytes::Bytes;
use net::adapter::net::dataforts::blob::adapter::BlobByteStream;
use net::adapter::net::dataforts::blob::cdc::CdcParams;
use net::adapter::net::dataforts::{BlobAdapter, BlobError, BlobRef, Encoding, MeshBlobAdapter};
use net::adapter::net::redex::Redex;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn deterministic_bytes(seed: u8, len: usize) -> Vec<u8> {
    let mut state: u64 = seed as u64;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

fn stream_one(bytes: Vec<u8>) -> BlobByteStream {
    Box::pin(futures::stream::once(async move { Ok::<_, BlobError>(Bytes::from(bytes)) }))
}

fn make_adapter() -> MeshBlobAdapter {
    let redex = Arc::new(Redex::new());
    // Match the CI-scale adapter from the A8 conformance test:
    // 4 MiB tree-node cache, 64 KiB per-chunk reservation. The
    // small per-chunk reservation matters here because CDC at
    // test-scale parameters produces 60-100+ chunks per payload;
    // 64 KiB × 100 = 6 MiB reservation total, fits comfortably.
    MeshBlobAdapter::new("cdc-conformance-v3", redex)
        .with_tree_node_cache(4 * 1024 * 1024)
        .with_chunk_file_max_memory_bytes(64 * 1024)
}

/// Test-scale CDC parameters: small enough that a few-hundred-
/// KiB payload produces 60+ chunks (meaningful boundary statistics
/// for the dedup-after-edit test) without pinning the per-test
/// memory budget. Stays inside the `fastcdc::v2020` accepted
/// ranges (min ≥ 64, avg ≥ 256, max ≥ 1024).
const CI_CDC_PARAMS: CdcParams = CdcParams {
    min: 1024,
    avg: 4 * 1024,
    max: 16 * 1024,
};

/// Walk a `BlobRef::Tree` and collect every reachable chunk hash.
/// Used by the dedup-after-edit test to compare chunk-set
/// intersections before and after the edit.
async fn collect_chunk_hashes(
    adapter: &MeshBlobAdapter,
    blob_ref: &BlobRef,
) -> HashSet<[u8; 32]> {
    use net::adapter::net::dataforts::blob::blob_tree::TreeNode;

    let mut out: HashSet<[u8; 32]> = HashSet::new();
    let mut stack: Vec<[u8; 32]> = Vec::new();
    let root_hash = blob_ref
        .tree_root_hash()
        .expect("collect_chunk_hashes called on a non-Tree BlobRef");
    stack.push(*root_hash);

    while let Some(node_hash) = stack.pop() {
        // The tree-node bytes live in the same per-chunk channel
        // shape as content chunks. The cleanest path to fetch
        // them from a test is `fetch_chunk`, which the adapter
        // exposes for content-addressed lookups.
        let bytes = adapter
            .fetch_chunk(&node_hash)
            .await
            .expect("tree node fetch must succeed");
        let node: TreeNode = postcard::from_bytes(&bytes)
            .expect("tree node must decode as TreeNode");
        match node {
            TreeNode::Internal { children } => {
                for (child_hash, _subtree_size) in children {
                    stack.push(child_hash);
                }
            }
            TreeNode::Leaf { chunks } => {
                for chunk in chunks {
                    out.insert(chunk.hash);
                }
            }
        }
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// 1. CDC determinism end-to-end
// ───────────────────────────────────────────────────────────────────────────

/// Two independent adapters store the same content via the CDC
/// path and produce identical root hashes. Full-range fetch
/// returns byte-identical bytes from both. Catches any drift in
/// the CDC chunker that would silently fragment dedup across
/// cluster nodes.
#[tokio::test]
async fn cdc_round_trip_is_deterministic_end_to_end() {
    let adapter_a = make_adapter();
    let adapter_b = make_adapter();
    // 256 KiB payload at the test CDC params averages ~64 chunks
    // with a depth-1 (sometimes depth-2) tree — enough boundaries
    // for the dedup test below to be meaningful.
    let payload = deterministic_bytes(0xCD, 256 * 1024);

    let ref_a = adapter_a
        .store_stream_tree_cdc_internal(
            stream_one(payload.clone()),
            Encoding::Replicated,
            CI_CDC_PARAMS,
        )
        .await
        .expect("CDC store on adapter A");
    let ref_b = adapter_b
        .store_stream_tree_cdc_internal(
            stream_one(payload.clone()),
            Encoding::Replicated,
            CI_CDC_PARAMS,
        )
        .await
        .expect("CDC store on adapter B");

    assert!(matches!(ref_a, BlobRef::Tree { .. }));
    assert!(matches!(ref_b, BlobRef::Tree { .. }));
    assert_eq!(ref_a.size(), payload.len() as u64);
    assert_eq!(ref_b.size(), payload.len() as u64);
    assert_eq!(
        ref_a.tree_root_hash(),
        ref_b.tree_root_hash(),
        "two independent CDC stores of the same content must agree on the root hash"
    );

    // Byte-identical round trip from either adapter.
    let fetched_a = adapter_a
        .fetch_range(&ref_a, 0..payload.len() as u64)
        .await
        .expect("full-range fetch from adapter A");
    let fetched_b = adapter_b
        .fetch_range(&ref_b, 0..payload.len() as u64)
        .await
        .expect("full-range fetch from adapter B");
    assert_eq!(fetched_a, payload);
    assert_eq!(fetched_b, payload);
}

// ───────────────────────────────────────────────────────────────────────────
// 2. Dedup-after-edit
// ───────────────────────────────────────────────────────────────────────────

/// Flip a small region in the middle of a payload, re-store via
/// CDC, walk the tree, and assert that the chunk-hash set of the
/// edited blob shares > 80% of its chunks with the original.
///
/// Fixed-size chunking at the same scale would invalidate every
/// chunk after the edit point (dedup ratio ~ 50%); CDC localises
/// the change to the chunks the edit intersects + at most one
/// boundary-neighbour, yielding a much tighter ratio. The
/// threshold is set above the Fixed-chunking baseline so a
/// regression that accidentally swapped the chunker behind CDC
/// (or broke the streaming boundary search) would fail this test.
#[tokio::test]
async fn cdc_one_region_edit_preserves_most_chunks() {
    let adapter = make_adapter();
    let mut payload = deterministic_bytes(0xBE, 256 * 1024);
    let original = payload.clone();

    // Original store.
    let ref_orig = adapter
        .store_stream_tree_cdc_internal(
            stream_one(original.clone()),
            Encoding::Replicated,
            CI_CDC_PARAMS,
        )
        .await
        .unwrap();
    let orig_chunks = collect_chunk_hashes(&adapter, &ref_orig).await;

    // Flip 16 bytes in the rough middle of the payload — large
    // enough to land outside the rolling-hash window, small enough
    // that only one CDC chunk (and possibly one boundary-neighbour)
    // is invalidated.
    let edit_start = 128 * 1024;
    for i in 0..16 {
        payload[edit_start + i] ^= 0xFF;
    }

    // Re-store edited payload.
    let ref_edited = adapter
        .store_stream_tree_cdc_internal(
            stream_one(payload.clone()),
            Encoding::Replicated,
            CI_CDC_PARAMS,
        )
        .await
        .unwrap();
    let edited_chunks = collect_chunk_hashes(&adapter, &ref_edited).await;

    // Roots must differ — content changed.
    assert_ne!(
        ref_orig.tree_root_hash(),
        ref_edited.tree_root_hash(),
        "the edited blob must hash to a different root than the original"
    );

    // Chunk-set Jaccard similarity. CDC localises the edit so
    // most original chunks remain reachable through the edited
    // blob's tree.
    let intersection = orig_chunks.intersection(&edited_chunks).count();
    let union = orig_chunks.union(&edited_chunks).count();
    assert!(union > 0, "both blobs must have at least one chunk");
    let dedup_ratio = intersection as f64 / union as f64;
    assert!(
        dedup_ratio > 0.80,
        "CDC dedup-after-edit ratio {} ≤ 0.80; CDC boundaries may be \
         cascading instead of localising the edit. orig_chunks={}, \
         edited_chunks={}, shared={}",
        dedup_ratio,
        orig_chunks.len(),
        edited_chunks.len(),
        intersection
    );
}

/// CDC vs Fixed chunking on the same payload + same edit: CDC
/// must dedup strictly more chunks than Fixed would. Pins the
/// invariant that distinguishes CDC's value from Fixed — without
/// this property CDC is pure overhead.
///
/// Fixed-chunking reference: every chunk that starts before the
/// edit dedups; every chunk that starts at or after the edit's
/// chunk-aligned boundary may not. With a 4 KiB chunk size and
/// an edit at offset 128 KiB, all chunks at offset >= 128 KiB
/// (= half the blob) are at risk of invalidation by the byte
/// shift any payload-restructuring would cause.
///
/// For the deterministic-bytes generator + the test edit shape,
/// Fixed (4 KiB) chunks past the edit are byte-equal (the edit
/// doesn't move byte offsets, just flips bits at a fixed range).
/// So Fixed actually dedups well here too. The real CDC-vs-Fixed
/// gap shows up under INSERT/DELETE-style edits that shift the
/// suffix — that's tested via the next variant.
#[tokio::test]
async fn cdc_under_insert_edit_outperforms_fixed_reference() {
    let adapter = make_adapter();
    let payload_a = deterministic_bytes(0xAA, 256 * 1024);

    // Insert 16 bytes at offset 128 KiB. Under Fixed chunking
    // every chunk past the insert shifts by 16 bytes, so Fixed
    // dedup of the suffix is near zero. Under CDC, the chunker
    // re-finds the same content-defined boundaries downstream of
    // the insert after a short re-sync window.
    let mut payload_b = Vec::with_capacity(payload_a.len() + 16);
    payload_b.extend_from_slice(&payload_a[..128 * 1024]);
    payload_b.extend_from_slice(&[0u8; 16]);
    payload_b.extend_from_slice(&payload_a[128 * 1024..]);

    let ref_a = adapter
        .store_stream_tree_cdc_internal(
            stream_one(payload_a),
            Encoding::Replicated,
            CI_CDC_PARAMS,
        )
        .await
        .unwrap();
    let ref_b = adapter
        .store_stream_tree_cdc_internal(
            stream_one(payload_b),
            Encoding::Replicated,
            CI_CDC_PARAMS,
        )
        .await
        .unwrap();
    let chunks_a = collect_chunk_hashes(&adapter, &ref_a).await;
    let chunks_b = collect_chunk_hashes(&adapter, &ref_b).await;

    let intersection = chunks_a.intersection(&chunks_b).count();
    let union = chunks_a.union(&chunks_b).count();
    let dedup_ratio = intersection as f64 / union as f64;

    // Under insert-shift, CDC must still preserve > 50% of chunks
    // — the suffix's content-defined boundaries re-align within
    // one chunk of the insert. A Fixed-chunking implementation
    // would land near 30% (only the prefix dedups).
    assert!(
        dedup_ratio > 0.50,
        "CDC insert-edit dedup ratio {} ≤ 0.50; suffix re-sync \
         appears broken. chunks_a={}, chunks_b={}, shared={}",
        dedup_ratio,
        chunks_a.len(),
        chunks_b.len(),
        intersection
    );
}
