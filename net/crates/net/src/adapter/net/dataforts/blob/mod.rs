//! Dataforts Phase 3 — content-addressable blob storage.
//!
//! Wraps a customer-supplied storage backend (S3 / IPFS / FS /
//! custom) behind a uniform [`BlobAdapter`] trait so the substrate
//! can ship event payloads larger than the inline-threshold via a
//! [`BlobRef`] pointer + a separate fetch path. Phase 3 of
//! `docs/misc/DATAFORTS_PLAN.md`.
//!
//! The substrate owns hash verification (BLAKE3) and the
//! discriminator byte that distinguishes inline vs blob-ref event
//! payloads. Lifecycle (refcounts, GC, retention) is delegated to
//! the customer's backend — S3 lifecycle policies, IPFS pinning,
//! etc. — by explicit locked decision.

pub mod adapter;
pub mod admission;
pub mod blob_ref;
pub mod conformance;
pub mod dispatch;
pub mod error;
pub mod fs;
pub mod mesh;
pub mod metrics;
pub mod migration;
pub mod noop;
pub mod publish_with_blob;
pub mod refcount;
pub mod registry;

/// Format a 32-byte content hash as the lowercase 64-char hex
/// string used throughout the blob layer for channel names,
/// `mesh://<hex>` URIs, log lines, and operator output. Single
/// definition shared by every module that needs the rendering —
/// `mesh.rs`, `migration.rs`, `metrics.rs`, etc.
pub(crate) fn hex32(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

pub use adapter::{BlobAdapter, BlobStat};
pub use admission::{
    auth_allows_blob_op, should_migrate_blob_to, should_pull_blob, MigrateBlobReject,
    MigrateBlobVerdict, PullBlobReject, PullBlobVerdict,
};
pub use blob_ref::{
    byte_range_to_chunks, chunk_payload, BlobRef, ChunkRangeRequest, ChunkRef, ChunkedPayload,
    Encoding, BLOB_CHUNK_SIZE_BYTES, BLOB_MANIFEST_BODY_VERSION, BLOB_MANIFEST_MAX_CHUNKS,
    BLOB_REF_DISCRIMINATOR, BLOB_REF_MAGIC, BLOB_REF_MAX_SIZE, BLOB_REF_SMALL_HEADER_LEN,
    BLOB_REF_VERSION_V1, BLOB_REF_VERSION_V2_MANIFEST,
};
pub use conformance::run_conformance_suite;
pub use dispatch::{
    classify_payload, publish_blob, publish_blob_ref, resolve_payload, EventPayload,
};
pub use error::BlobError;
pub use fs::FileSystemAdapter;
pub use mesh::{MeshBlobAdapter, DEFAULT_BLOB_HEAT_HALF_LIFE};
pub use metrics::{
    evaluate_health_gate, BlobMetrics, BlobMetricsSnapshot, HealthGateAction,
    HEALTH_GATE_CLEAR_THRESHOLD, HEALTH_GATE_EMIT_THRESHOLD,
};
pub use migration::{
    drive_blob_migration_tick, drive_blob_migration_tick_arc,
    drive_blob_migration_tick_with_manifest_resolver, parse_blob_heat_tag, BlobMigrationCandidate,
    BlobMigrationController, BlobMigrationTickReport, ManifestSiblings,
};
pub use noop::NoopAdapter;
pub use publish_with_blob::{publish_with_blob, BlobDurability, PublishWithBlobReceipt};
pub use refcount::{should_sweep, BlobRefcountTable, RefcountEntry, DEFAULT_RETENTION_FLOOR};
pub use registry::{global_blob_adapter_registry, BlobAdapterRegistry, BlobAdapterRegistryError};
