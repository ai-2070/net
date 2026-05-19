//! Phase C conformance test for the v0.3 Reed-Solomon
//! `Encoding::ReedSolomon { k, m }` store + fetch path.
//!
//! Pins the three end-to-end correctness contracts Phase C's
//! plan calls load-bearing:
//!
//! 1. **RS round trip with all chunks present** — store a
//!    multi-stripe blob via the RS path, fetch every byte back,
//!    assert byte-equality. Catches any drift in the striper or
//!    leaf-encoding path.
//! 2. **RS reconstruction tolerates m chunk losses per stripe** —
//!    delete exactly `m` data chunks from each stripe, fetch
//!    succeeds via parity-driven reconstruction, bytes match.
//!    Pins the C5 read-side recovery contract.
//! 3. **RS reconstruction fails cleanly at m + 1 losses** —
//!    delete `m + 1` chunks from one stripe, fetch surfaces a
//!    typed `BlobError::Backend("erasure: stripe unrecoverable
//!    …")` instead of corrupting the read or panicking.
//!
//! All three run by default at memory-feasible scale (kilobyte-
//! scale RS params via the test-only
//! `store_stream_tree_rs_internal` helper).
//!
//! Run: `cargo test --features dataforts --test
//! dataforts_blob_v3_rs_conformance`

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
    MeshBlobAdapter::new("rs-conformance-v3", redex)
        .with_tree_node_cache(4 * 1024 * 1024)
        .with_chunk_file_max_memory_bytes(64 * 1024)
}

/// RS(k=3, m=2) at 1 KiB chunks: each stripe = 3 KiB data + 2
/// parity. Small enough for the test runner; large enough to
/// exercise multi-stripe blobs.
const CI_RS_PARAMS: RsParams = RsParams { k: 3, m: 2 };
const CI_CHUNK_SIZE: u32 = 1024;

/// Walk the tree and return every ErasureLeaf the blob touches.
/// Used to enumerate stripes for the "delete chunks per stripe"
/// tests.
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
                panic!("RS conformance: did not expect a Replicated Leaf in an RS-encoded tree");
            }
            TreeNode::ErasureLeaf { stripes } => {
                leaves.push(stripes);
            }
        }
    }
    leaves
}

// ───────────────────────────────────────────────────────────────────────────
// 1. RS round trip — all chunks present
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rs_round_trip_with_all_chunks_present() {
    let adapter = make_adapter();
    // 12 chunks = 4 full RS(3, 2) stripes. Each stripe = 3 data + 2 parity.
    let payload = deterministic_bytes(0xC9, CI_CHUNK_SIZE as usize * 12);
    let blob_ref = adapter
        .store_stream_tree_rs_internal(
            stream_one(payload.clone()),
            ChunkingStrategy::Fixed {
                size: CI_CHUNK_SIZE,
            },
            CI_RS_PARAMS,
        )
        .await
        .expect("RS store_stream_tree round trip");

    assert!(matches!(blob_ref, BlobRef::Tree { .. }));
    assert_eq!(blob_ref.size(), payload.len() as u64);
    assert_eq!(
        blob_ref.encoding(),
        Some(Encoding::ReedSolomon { k: 3, m: 2 })
    );

    let fetched = adapter
        .fetch_range(&blob_ref, 0..payload.len() as u64)
        .await
        .expect("RS fetch_range");
    assert_eq!(fetched, payload, "RS round-trip must be byte-identical");

    // Conformance: every leaf is an ErasureLeaf with the full
    // RS shape. No Replicated stripes because the payload
    // divides evenly into the 3-chunk stripe boundary.
    let leaves = collect_erasure_leaves(&adapter, &blob_ref).await;
    assert!(!leaves.is_empty(), "must have at least one ErasureLeaf");
    for stripes in &leaves {
        for stripe in stripes {
            stripe.validate().expect("stripe validates");
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// 2. RS reconstruction tolerates m chunk losses per stripe
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rs_reconstruction_tolerates_m_losses_per_stripe() {
    let adapter = make_adapter();
    // 9 chunks = 3 full RS(3, 2) stripes — clean stripe
    // boundary, no trailing Replicated partial.
    let payload = deterministic_bytes(0xCA, CI_CHUNK_SIZE as usize * 9);
    let blob_ref = adapter
        .store_stream_tree_rs_internal(
            stream_one(payload.clone()),
            ChunkingStrategy::Fixed {
                size: CI_CHUNK_SIZE,
            },
            CI_RS_PARAMS,
        )
        .await
        .unwrap();

    // Delete exactly m = 2 data chunks from each RS stripe.
    let leaves = collect_erasure_leaves(&adapter, &blob_ref).await;
    let mut deleted_count = 0usize;
    for stripes in &leaves {
        for stripe in stripes {
            // Only act on RS stripes (skip any Replicated trailing
            // partial — it has no parity to reconstruct from, and
            // the test deliberately uses a chunk count divisible
            // by k to avoid that case).
            if !matches!(stripe.encoding, Encoding::ReedSolomon { .. }) {
                continue;
            }
            let data_hashes: Vec<[u8; 32]> = stripe
                .chunks
                .iter()
                .filter(|c| c.is_data())
                .map(|c| c.hash)
                .collect();
            // Delete the first m = 2 data chunks per stripe.
            for hash in &data_hashes[0..2] {
                adapter.delete_chunk(hash).await.unwrap();
                deleted_count += 1;
            }
        }
    }
    assert!(deleted_count > 0, "must have deleted at least one chunk");

    // Fetch still succeeds via reconstruction.
    let fetched = adapter
        .fetch_range(&blob_ref, 0..payload.len() as u64)
        .await
        .expect("RS fetch must reconstruct after m-chunk loss per stripe");
    assert_eq!(fetched, payload, "reconstructed bytes must match original");
}

// ───────────────────────────────────────────────────────────────────────────
// 3. RS reconstruction fails cleanly at m + 1 losses
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rs_reconstruction_fails_cleanly_when_more_than_m_chunks_lost() {
    let adapter = make_adapter();
    // Single stripe — 3 chunks at RS(3, 2).
    let payload = deterministic_bytes(0xCB, CI_CHUNK_SIZE as usize * 3);
    let blob_ref = adapter
        .store_stream_tree_rs_internal(
            stream_one(payload.clone()),
            ChunkingStrategy::Fixed {
                size: CI_CHUNK_SIZE,
            },
            CI_RS_PARAMS,
        )
        .await
        .unwrap();

    let leaves = collect_erasure_leaves(&adapter, &blob_ref).await;
    let rs_stripe: &StripeBlock = leaves
        .iter()
        .flatten()
        .find(|s| matches!(s.encoding, Encoding::ReedSolomon { .. }))
        .expect("must have an RS stripe");
    // Delete m + 1 = 3 chunks (any mix of data + parity).
    let all_hashes: Vec<[u8; 32]> = rs_stripe.chunks.iter().map(|c| c.hash).collect();
    for hash in &all_hashes[0..3] {
        adapter.delete_chunk(hash).await.unwrap();
    }

    let err = adapter
        .fetch_range(&blob_ref, 0..payload.len() as u64)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unrecoverable") || msg.contains("erasure"),
        "expected unrecoverable-stripe error, got: {}",
        msg
    );
}

// ───────────────────────────────────────────────────────────────────────────
// 4. RS determinism — two independent stores agree on root hash
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rs_two_adapters_same_payload_agree_on_root() {
    let adapter_a = make_adapter();
    let adapter_b = make_adapter();
    let payload = deterministic_bytes(0xCC, CI_CHUNK_SIZE as usize * 9);
    let r_a = adapter_a
        .store_stream_tree_rs_internal(
            stream_one(payload.clone()),
            ChunkingStrategy::Fixed {
                size: CI_CHUNK_SIZE,
            },
            CI_RS_PARAMS,
        )
        .await
        .unwrap();
    let r_b = adapter_b
        .store_stream_tree_rs_internal(
            stream_one(payload),
            ChunkingStrategy::Fixed {
                size: CI_CHUNK_SIZE,
            },
            CI_RS_PARAMS,
        )
        .await
        .unwrap();
    assert_eq!(
        r_a.tree_root_hash(),
        r_b.tree_root_hash(),
        "RS produces deterministic root across independent adapters"
    );
}
