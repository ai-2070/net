//! Phase A conformance test for the v0.3 `BlobRef::Tree` track.
//!
//! Pins the end-to-end correctness contract Phase A (A1–A6)
//! delivers when composed:
//!
//! 1. **Byte-identical round trip** at a realistic-but-feasible
//!    scale — store a deterministic-content blob via
//!    `store_stream_tree`, fetch every chunk back via
//!    `fetch_range`, assert byte-equality.
//! 2. **Tree shape verification** — depth in range, root + every
//!    leaf locally reachable, per-node BLAKE3 verification, no
//!    manifest-body explosion (the entire tree path stays under
//!    a few-MiB ceiling even for a multi-GiB blob).
//! 3. **Manifest LRU cache effectiveness** — repeated range
//!    reads on the same tree paths observe a >90% cache hit
//!    ratio after warmup. Pins the A5 cache against accidental
//!    regression.
//! 4. **Determinism across runs** — two independent stores of
//!    the same content produce identical root hashes.
//!
//! The default-scale test runs in CI (small chunk size + ~100
//! MiB synthetic payload, ~30s on a dev workstation). An
//! `#[ignore]`'d real-scale companion drives the same shape at
//! 100 GiB; operators run it via `cargo test -- --ignored` when
//! validating a release.
//!
//! Run default: `cargo test --features dataforts --test
//! dataforts_blob_v3_tree_conformance`
//! Run real scale: append ` -- --ignored`

#![cfg(feature = "dataforts")]

use std::sync::Arc;

use bytes::Bytes;
use net::adapter::net::dataforts::blob::adapter::BlobByteStream;
use net::adapter::net::dataforts::blob::blob_tree::{
    ChunkingStrategy, MAX_TREE_DEPTH, TREE_THRESHOLD_BYTES,
};
use net::adapter::net::dataforts::{BlobAdapter, BlobError, BlobRef, Encoding, MeshBlobAdapter};
use net::adapter::net::redex::Redex;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Deterministic LCG-derived payload — no `rand` dep, reproducible
/// across runs, content-distinct per `seed`.
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
    // 4 MiB cache: plenty for the tens of nodes a small tree
    // walks, small enough that the cache allocation itself isn't
    // the dominant test memory line.
    //
    // 64 KiB per-chunk reservation: production default is 64 MiB,
    // which would multiply by the chunk count (thousands here)
    // and exceed the test runner's commit limit. The live byte
    // count per chunk file is a single chunk (≤ chunk size), so
    // 64 KiB leaves plenty of headroom while keeping the
    // reservation total inside ~140 MiB for a 2 K-chunk tree.
    MeshBlobAdapter::new("conformance-v3", redex)
        .with_tree_node_cache(4 * 1024 * 1024)
        .with_chunk_file_max_memory_bytes(64 * 1024)
}

/// Default-scale parameters. Sized so the test fits in CI's
/// per-test memory budget (~80 MiB peak with two adapters +
/// stored chunks + fetch buffer) and finishes well under a
/// minute on a dev workstation. The chunk size is far below the
/// production 4 MiB so a FANOUT-spanning tree exercises without
/// gigabyte-scale allocations; chunk size stays ≥ 8 KiB so the
/// "manifest body < 1 % of payload" invariant the test asserts
/// is actually achievable (per-chunk metadata is ~50 B, so chunk
/// size much below 5 KiB makes manifest body exceed 1 %).
const CI_CHUNK_SIZE: u32 = 8 * 1024; // 8 KiB
const CI_BLOB_BYTES: usize = 16 * 1024 * 1024; // 16 MiB → 2 K chunks → 16 leaves + 1 root

// ───────────────────────────────────────────────────────────────────────────
// Default-scale conformance test
// ───────────────────────────────────────────────────────────────────────────

/// End-to-end round trip + tree shape + cache effectiveness at
/// the CI-feasible scale. Runs by default.
#[tokio::test]
async fn tree_v0_3_phase_a_conformance_at_ci_scale() {
    let adapter = make_adapter();
    let payload = deterministic_bytes(0xC0, CI_BLOB_BYTES);

    // ── 1. Store via store_stream_tree_internal (chunk size
    //       tunable for CI). The public surface
    //       `store_stream_tree` would require 4 MiB chunks; we
    //       go through the test-internal helper to keep the CI
    //       memory bound reasonable.
    let blob_ref = adapter
        .store_stream_tree_internal(
            stream_one(payload.clone()),
            Encoding::Replicated,
            CI_CHUNK_SIZE,
        )
        .await
        .expect("store_stream_tree round trip");

    // Returned ref is a Tree with the right shape.
    assert!(matches!(blob_ref, BlobRef::Tree { .. }));
    assert_eq!(blob_ref.size(), CI_BLOB_BYTES as u64);
    let depth = blob_ref.tree_depth().expect("tree depth");
    assert!((2..=MAX_TREE_DEPTH).contains(&depth), "depth {depth} out of expected range");

    // ── 2. Determinism — a second store of the same content
    //       lands at the SAME root hash.
    let adapter_b = make_adapter();
    let blob_ref_b = adapter_b
        .store_stream_tree_internal(
            stream_one(payload.clone()),
            Encoding::Replicated,
            CI_CHUNK_SIZE,
        )
        .await
        .unwrap();
    assert_eq!(
        blob_ref.tree_root_hash(),
        blob_ref_b.tree_root_hash(),
        "two independent stores of the same content must produce identical root hashes"
    );

    // ── 3. Full-range round-trip — fetch the whole blob back
    //       and assert byte-equality.
    let fetched = adapter
        .fetch_range(&blob_ref, 0..CI_BLOB_BYTES as u64)
        .await
        .expect("full-range fetch");
    assert_eq!(fetched.len(), CI_BLOB_BYTES);
    assert_eq!(fetched, payload, "byte-identical round trip");

    // ── 4. Cache effectiveness — repeat 16 small random range
    //       reads. After warmup, hit ratio should exceed 90%
    //       because all the spanning manifest nodes land in the
    //       cache on first walk.
    let _ = adapter.tree_node_cache_stats(); // baseline
    // First pass: prime the cache by reading every leaf-spanning
    // range once.
    for i in 0..16 {
        let start = (i * (CI_BLOB_BYTES / 16)) as u64;
        let end = start + 4096;
        let _ = adapter.fetch_range(&blob_ref, start..end).await.unwrap();
    }
    let (priming_hits, priming_misses, _, _) = adapter.tree_node_cache_stats().unwrap();
    // Second pass: identical reads. Every manifest fetch should
    // hit; cache effectiveness measured.
    for i in 0..16 {
        let start = (i * (CI_BLOB_BYTES / 16)) as u64;
        let end = start + 4096;
        let _ = adapter.fetch_range(&blob_ref, start..end).await.unwrap();
    }
    let (final_hits, final_misses, _, _) = adapter.tree_node_cache_stats().unwrap();
    let pass2_hits = final_hits - priming_hits;
    let pass2_misses = final_misses - priming_misses;
    let pass2_total = pass2_hits + pass2_misses;
    assert!(pass2_total > 0, "second pass must do some cache work");
    let hit_ratio = pass2_hits as f64 / pass2_total as f64;
    assert!(
        hit_ratio > 0.90,
        "second-pass cache hit ratio {} below 90%; the cache may have evicted prematurely \
         or the manifest LRU isn't being consulted on the walk path",
        hit_ratio
    );

    // ── 5. Tree-walk integrity — the tree we stored verifies
    //       end-to-end. A range fetch that visits every node
    //       implicitly BLAKE3-verifies each.
    //       (Already covered by step 3's successful return.)

    // ── 6. Manifest-body explosion guard — the TOTAL bytes of
    //       all reachable tree-node blobs (root + leaves +
    //       internals) must be a tiny fraction of the blob's
    //       payload size. Pin at < 1% to catch any regression
    //       where TreeNode encoding bloats unexpectedly.
    let (_, _, cache_bytes, cache_entries) = adapter.tree_node_cache_stats().unwrap();
    assert!(
        cache_bytes < CI_BLOB_BYTES / 100,
        "cached tree-node bytes ({} for {} entries) exceed 1% of payload \
         ({}); manifest-body explosion?",
        cache_bytes,
        cache_entries,
        CI_BLOB_BYTES
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Real-scale companion test
// ───────────────────────────────────────────────────────────────────────────

/// Same shape as the CI conformance, but at the spec'd 100 GiB
/// scale using the production 4 MiB chunk size. Allocates ~100
/// GiB of heap-backed chunks plus the manifest tree — requires
/// a machine with >100 GiB RAM and several hours of wall time.
///
/// Ignored by default; operators run before tagging a v0.3
/// release. Surfaces real-world performance + correctness at
/// the scale the plan promises.
///
/// Run: `cargo test --features dataforts --test
/// dataforts_blob_v3_tree_conformance \
///       tree_v0_3_phase_a_conformance_at_100_gib_scale -- --ignored`
#[tokio::test]
#[ignore = "real-scale: 100 GiB; needs >100 GiB RAM; run via --ignored"]
async fn tree_v0_3_phase_a_conformance_at_100_gib_scale() {
    let adapter = make_adapter();
    // 100 GiB payload at production 4 MiB chunk size → 25,600
    // chunks. At fanout 128, that's 200 full leaves under one
    // depth-2 internal → root_depth = 2.
    let total_bytes: usize = 100 * 1024 * 1024 * 1024;
    let payload = deterministic_bytes(0xFF, total_bytes);

    // Sanity: above TREE_THRESHOLD_BYTES so the producer hint
    // says Tree.
    assert!(total_bytes as u64 >= TREE_THRESHOLD_BYTES);

    // Default ChunkingStrategy is Fixed { size: BLOB_CHUNK_SIZE_BYTES }
    // (4 MiB) which is what the production path accepts.
    let blob_ref = adapter
        .store_stream_tree(
            stream_one(payload.clone()),
            Encoding::Replicated,
            ChunkingStrategy::default(),
        )
        .await
        .expect("100 GiB store_stream_tree");

    assert!(matches!(blob_ref, BlobRef::Tree { .. }));
    assert_eq!(blob_ref.size(), total_bytes as u64);
    assert_eq!(blob_ref.tree_depth(), Some(2));

    // Round trip — assert byte-equality on a few random ranges.
    // A full-range fetch would allocate another 100 GiB.
    for offset in [0u64, 1 << 30, 50u64 * (1 << 30), total_bytes as u64 - 4096] {
        let end = offset.saturating_add(4096).min(total_bytes as u64);
        let fetched = adapter.fetch_range(&blob_ref, offset..end).await.unwrap();
        assert_eq!(
            fetched,
            &payload[offset as usize..end as usize],
            "range fetch mismatch at offset {}",
            offset
        );
    }
}
