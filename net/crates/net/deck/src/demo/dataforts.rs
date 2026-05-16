//! Per-node `MeshBlobAdapter` instances for the demo. Each
//! node gets one in-memory `Redex`-backed adapter at boot,
//! seeded with a small number of synthetic blobs so the
//! DATAFORTS tab + BLOBS tab render real records (real
//! refcounts, real first-seen timestamps) instead of the
//! synthetic-snapshot fixtures the legacy `samples` feature
//! installed.
//!
//! v1 keeps the adapters write-once at boot. A follow-up
//! slice ties a periodic blob writer to the heartbeat loop
//! so the BLOBS tab's "first seen Xs ago" column ticks
//! forward over a long demo session.

use std::sync::Arc;

use net_sdk::dataforts::{publish_blob_ref, BlobAdapter, MeshBlobAdapter, Redex};

/// Disk-cap presets per demo node, chosen so the DATAFORTS
/// tab's `DISK` bar renders at visibly distinct fill
/// percentages across the 9 nodes. Indexed by node position
/// in the harness; falls back to the last entry for any
/// out-of-range index. Profiles are tagged with role names
/// the operator will recognize from a real cluster — primary
/// / cold / replicated / analytics / backup / ingest / etc.
const ADAPTER_PROFILES: &[(&str, u64, usize, usize)] = &[
    // (id, disk_cap_bytes, initial_stores, initial_fetches)
    ("primary", 1u64 << 40, 5, 3),       // 1 TiB cap, balanced
    ("cold", 512u64 << 30, 2, 18),       // 512 GiB cap, read-heavy
    ("replicated", 2u64 << 40, 11, 0),   // 2 TiB cap, write-heavy
    ("analytics", 768u64 << 30, 7, 5),   // 768 GiB cap, balanced
    ("backup", 4u64 << 40, 3, 0),        // 4 TiB cap, write-only
    ("ingest", 1u64 << 40, 14, 1),       // 1 TiB cap, ingest-heavy
    ("snapshot", 2u64 << 40, 4, 22),     // 2 TiB cap, snapshot-restore heavy
    ("staging", 256u64 << 30, 8, 8),     // 256 GiB cap, balanced small
    ("archive", 8u64 << 40, 1, 0),       // 8 TiB cap, write-once
];

/// Build N adapters — one per demo node. Returns a vec of
/// shared handles the harness keeps for the session and that
/// the deck's DATAFORTS tab iterates.
pub async fn build_adapters(n: usize) -> Vec<Arc<MeshBlobAdapter>> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (id, cap, stores, fetches) = ADAPTER_PROFILES
            .get(i)
            .copied()
            .unwrap_or(*ADAPTER_PROFILES.last().unwrap());
        out.push(install_one(id, cap, stores, fetches).await);
    }
    out
}

async fn install_one(
    id: &str,
    cap_bytes: u64,
    stores: usize,
    fetches: usize,
) -> Arc<MeshBlobAdapter> {
    let redex = Arc::new(Redex::new());
    let adapter = MeshBlobAdapter::new(id, redex).with_disk_capacity(cap_bytes);
    let adapter = Arc::new(adapter);

    // Synthetic content varies per (id, index) so each
    // adapter's chunks hash distinctly. The id is folded into
    // the payload prefix so cross-adapter dedup wouldn't
    // collapse them either.
    let mut stored = Vec::with_capacity(stores);
    for i in 0..stores {
        let payload = format!("{id}/blob-{i:03}-demo-content").into_bytes();
        if let Ok(blob) =
            publish_blob_ref(adapter.as_ref(), format!("mesh://{id}/{i}"), &payload).await
        {
            stored.push(blob);
        }
    }
    // Fire `fetches` re-fetches of the first stored blob so the
    // adapter's `blobs_fetched` counter isn't zero (which would
    // make the DATAFORTS row read as a write-only tier).
    if let Some(blob) = stored.first() {
        for _ in 0..fetches {
            let _ = BlobAdapter::fetch(adapter.as_ref(), blob).await;
        }
    }
    adapter
}
