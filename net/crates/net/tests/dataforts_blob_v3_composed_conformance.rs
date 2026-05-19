//! Tree × Reed-Solomon × CDC composed-conformance test.
//!
//! The per-axis conformance harnesses (tree, rs, cdc) each
//! exercise one v0.3 axis at a time:
//!
//! - `dataforts_blob_v3_tree_conformance.rs` — Tree + Replicated
//!   + Fixed chunking
//! - `dataforts_blob_v3_rs_conformance.rs` — Tree + RS + Fixed
//!   chunking
//! - `dataforts_blob_v3_cdc_conformance.rs` — Tree + Replicated
//!   + CDC chunking
//!
//! What none of them cover is the production-loaded shape:
//! Tree + Reed-Solomon + content-defined chunking, with stripe
//! members produced by the FastCDC chunker rather than fixed-size
//! cuts. That path is what a TB-scale RS-encoded archival blob
//! actually exercises — variable-size data chunks flowing into
//! the striper, the striper closing at `k = 3` chunks regardless
//! of byte target, parity computed against the unevenly-sized
//! shards.
//!
//! This file pins three composed contracts:
//!
//! 1. **Round trip Tree × RS × CDC** — store + fetch + byte-equal.
//! 2. **Reconstruction across the composition** — delete `m`
//!    data chunks per stripe; fetch_range still reconstructs
//!    byte-identical output. The reconstruction path must
//!    handle uneven CDC shard sizes (padding-trim logic).
//! 3. **Unrecoverable at m+1** — delete `m+1` chunks from one
//!    stripe; the fetch surfaces a typed
//!    `BlobError::Backend("erasure: stripe unrecoverable ...")`
//!    without panicking or returning corrupted bytes.
//!
//! Run: `cargo test --features dataforts --test
//! dataforts_blob_v3_composed_conformance`

#![cfg(feature = "dataforts")]

use std::sync::Arc;

use bytes::Bytes;
use net::adapter::net::dataforts::blob::adapter::BlobByteStream;
use net::adapter::net::dataforts::blob::blob_tree::{ChunkingStrategy, StripeBlock, TreeNode};
use net::adapter::net::dataforts::blob::erasure::RsParams;
use net::adapter::net::dataforts::{BlobAdapter, BlobError, BlobRef, Encoding, MeshBlobAdapter};
use net::adapter::net::redex::Redex;

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
    Box::pin(futures::stream::once(async move {
        Ok::<_, BlobError>(Bytes::from(bytes))
    }))
}

fn make_adapter() -> MeshBlobAdapter {
    let redex = Arc::new(Redex::new());
    MeshBlobAdapter::new("composed-conformance-v3", redex)
        .with_tree_node_cache(4 * 1024 * 1024)
        .with_chunk_file_max_memory_bytes(64 * 1024)
}

/// CDC params sized for the test runner — content-defined cuts
/// happen in the [256, 4096] byte range. Pinned to the same
/// triple as `cdc::tests::TEST_PARAMS` so the chunker behavior
/// is shared with the per-axis CDC conformance.
const CI_CDC_MIN: u32 = 256;
const CI_CDC_AVG: u32 = 1024;
const CI_CDC_MAX: u32 = 4096;

/// RS(k=3, m=2). 3 data + 2 parity per stripe; tolerates any 2
/// chunk losses per stripe and fails cleanly at 3.
const CI_RS_PARAMS: RsParams = RsParams { k: 3, m: 2 };

/// Walk the Tree and return every `ErasureLeaf` it touches.
/// Borrowed verbatim from `dataforts_blob_v3_rs_conformance.rs`
/// — the v0.3 plan stipulates an RS-encoded Tree emits
/// ErasureLeaf nodes at the bottom of every descent path.
async fn collect_erasure_leaves(
    adapter: &MeshBlobAdapter,
    blob_ref: &BlobRef,
) -> Vec<Vec<StripeBlock>> {
    let root_hash = *blob_ref
        .tree_root_hash()
        .expect("collect_erasure_leaves called on a non-Tree BlobRef");
    let mut leaves: Vec<Vec<StripeBlock>> = Vec::new();
    let mut stack: Vec<[u8; 32]> = vec![root_hash];
    while let Some(node_hash) = stack.pop() {
        let bytes = adapter.fetch_chunk(&node_hash).await.expect("node fetch");
        let node = TreeNode::decode(&bytes).expect("node decode");
        match node {
            TreeNode::Internal { children } => {
                for (child_hash, _) in children {
                    stack.push(child_hash);
                }
            }
            TreeNode::Leaf { .. } => {
                panic!("composed conformance: RS-encoded Tree must NOT contain a Replicated Leaf");
            }
            TreeNode::ErasureLeaf { stripes } => {
                leaves.push(stripes);
            }
        }
    }
    leaves
}

#[tokio::test]
async fn tree_rs_cdc_round_trip_byte_equal() {
    let adapter = make_adapter();
    // 24 KiB — well above CDC's `min` (256) and large enough to
    // produce multiple CDC chunks at avg=1 KiB; the striper
    // then groups them into multiple stripes.
    let payload = deterministic_bytes(0xE0, 24 * 1024);
    let blob_ref = adapter
        .store_stream_tree_rs_internal(
            stream_one(payload.clone()),
            ChunkingStrategy::Cdc {
                min: CI_CDC_MIN,
                avg: CI_CDC_AVG,
                max: CI_CDC_MAX,
            },
            CI_RS_PARAMS,
        )
        .await
        .expect("store_stream_tree_rs_internal with CDC");
    assert!(matches!(blob_ref, BlobRef::Tree { .. }));
    assert_eq!(blob_ref.size(), payload.len() as u64);
    assert_eq!(
        blob_ref.encoding(),
        Some(Encoding::ReedSolomon { k: 3, m: 2 }),
    );
    // Round-trip the full range — bytes must match the input
    // exactly.
    let fetched = adapter
        .fetch_range(&blob_ref, 0..payload.len() as u64)
        .await
        .expect("fetch_range round trip");
    assert_eq!(
        fetched, payload,
        "Tree × RS × CDC round trip must be byte-identical"
    );

    // Conformance: at least one ErasureLeaf in the descent (the
    // payload is large enough that the striper closes at least
    // one stripe).
    let leaves = collect_erasure_leaves(&adapter, &blob_ref).await;
    assert!(
        !leaves.is_empty(),
        "RS+CDC blob must produce at least one ErasureLeaf"
    );
}

#[tokio::test]
async fn tree_rs_cdc_reconstructs_after_m_losses_per_stripe() {
    let adapter = make_adapter();
    let payload = deterministic_bytes(0xE1, 24 * 1024);
    let blob_ref = adapter
        .store_stream_tree_rs_internal(
            stream_one(payload.clone()),
            ChunkingStrategy::Cdc {
                min: CI_CDC_MIN,
                avg: CI_CDC_AVG,
                max: CI_CDC_MAX,
            },
            CI_RS_PARAMS,
        )
        .await
        .unwrap();

    // Delete exactly `m = 2` data chunks from each RS stripe.
    let leaves = collect_erasure_leaves(&adapter, &blob_ref).await;
    let mut deletions = 0usize;
    for stripes in &leaves {
        for stripe in stripes {
            if !matches!(stripe.encoding, Encoding::ReedSolomon { .. }) {
                // Trailing partial stripe falls back to
                // Replicated — skip; it has no parity model.
                continue;
            }
            let data_hashes: Vec<[u8; 32]> = stripe
                .chunks
                .iter()
                .filter(|c| c.is_data())
                .map(|c| c.hash)
                .collect();
            // Delete 2 data chunks — the `m=2` tolerance.
            // CDC produces variable-size shards, so this also
            // exercises the reconstruction padding-trim path.
            for h in &data_hashes[0..2] {
                adapter.delete_chunk(h).await.unwrap();
                deletions += 1;
            }
        }
    }
    assert!(
        deletions > 0,
        "must have deleted at least m chunks from at least one RS stripe",
    );

    // fetch_range must reconstruct byte-identical output.
    let fetched = adapter
        .fetch_range(&blob_ref, 0..payload.len() as u64)
        .await
        .expect("RS+CDC fetch must reconstruct after m-chunk loss per stripe");
    assert_eq!(
        fetched, payload,
        "Tree × RS × CDC reconstructed bytes must match original"
    );
}

#[tokio::test]
async fn tree_rs_cdc_fails_cleanly_when_more_than_m_chunks_lost() {
    let adapter = make_adapter();
    let payload = deterministic_bytes(0xE2, 16 * 1024);
    let blob_ref = adapter
        .store_stream_tree_rs_internal(
            stream_one(payload.clone()),
            ChunkingStrategy::Cdc {
                min: CI_CDC_MIN,
                avg: CI_CDC_AVG,
                max: CI_CDC_MAX,
            },
            CI_RS_PARAMS,
        )
        .await
        .unwrap();

    // Locate any RS stripe and kill m+1 = 3 of its members.
    let leaves = collect_erasure_leaves(&adapter, &blob_ref).await;
    let mut killed = false;
    'outer: for stripes in &leaves {
        for stripe in stripes {
            if !matches!(stripe.encoding, Encoding::ReedSolomon { .. }) {
                continue;
            }
            let all_hashes: Vec<[u8; 32]> = stripe.chunks.iter().map(|c| c.hash).collect();
            for h in &all_hashes[0..3] {
                adapter.delete_chunk(h).await.unwrap();
            }
            killed = true;
            break 'outer;
        }
    }
    assert!(killed, "must have found at least one RS stripe to degrade");

    // Fetch must surface a typed error, not corrupt the bytes or
    // panic.
    let err = adapter
        .fetch_range(&blob_ref, 0..payload.len() as u64)
        .await
        .expect_err("m+1 losses must surface a typed error");
    let msg = err.to_string();
    assert!(
        msg.contains("unrecoverable") || msg.contains("erasure"),
        "expected unrecoverable-stripe error, got: {}",
        msg
    );
}
