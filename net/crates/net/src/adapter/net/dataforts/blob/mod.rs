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
pub mod bandwidth;
pub mod blob_ref;
pub mod blob_tree;
pub mod blob_tree_cache;
pub mod cdc;
pub mod conformance;
pub mod dispatch;
pub mod erasure;
pub mod error;
pub mod fs;
pub mod mesh;
pub mod metrics;
pub mod migration;
pub mod noop;
pub mod overflow;
pub mod publish_with_blob;
pub mod refcount;
pub mod registry;
pub mod stripe_index;

/// Format a 32-byte content hash as the lowercase 64-char hex
/// string used throughout the blob layer for channel names,
/// `mesh://<hex>` URIs, log lines, and operator output. Single
/// definition shared by every module that needs the rendering —
/// `mesh.rs`, `migration.rs`, `metrics.rs`, etc.
///
/// Lookup-table-based hex encoding per dataforts perf #171 —
/// pre-fix did 32 dispatches through `core::fmt::Arguments` via
/// `write!("{:02x}", b)`, which is ~10× slower than the table
/// form below for the same output. Hot enough on the bulk-fetch
/// path (one call per chunk for `chunk_channel`) and the
/// per-error-string path (`mesh://<hex>` URIs) that the saving
/// adds up.
#[inline]
pub(crate) fn hex32(hash: &[u8; 32]) -> String {
    let mut buf = [0u8; 64];
    hex32_into(hash, &mut buf);
    // `hex32_into` only writes bytes from `HEX_LOWER` (all ASCII),
    // so the buffer is valid UTF-8 by construction. Using
    // `from_utf8` (validating) keeps the surface safe; the
    // validator is a SIMD-fast no-op for short ASCII.
    #[expect(
        clippy::expect_used,
        reason = "hex32_into writes only ASCII bytes from HEX_LOWER; from_utf8 is infallible by construction"
    )]
    String::from_utf8(buf.to_vec()).expect("hex output is ASCII by construction")
}

/// Lookup table for lowercase hex digits. Used by [`hex32_into`].
const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

/// Encode `hash` as lowercase hex into the caller-owned `dst`
/// buffer. Zero-allocation form of [`hex32`]; the caller decides
/// where the 64 output bytes live.
///
/// Useful when the bytes feed straight into another buffer
/// (e.g. building a channel name with a prefix in front of the
/// hex). Per dataforts perf #171, hot callers should reach for
/// this form so the hex render doesn't pay the `String` alloc
/// each time.
#[inline]
pub(crate) fn hex32_into(hash: &[u8; 32], dst: &mut [u8; 64]) {
    for (i, &b) in hash.iter().enumerate() {
        dst[i * 2] = HEX_LOWER[(b >> 4) as usize];
        dst[i * 2 + 1] = HEX_LOWER[(b & 0x0f) as usize];
    }
}

pub use adapter::{BlobAdapter, BlobInventoryEntry, BlobListOptions, BlobStat};
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
pub use mesh::{
    MeshBlobAdapter, OverflowConfig, RepairReport, DEFAULT_BLOB_HEAT_HALF_LIFE,
    DEFAULT_OVERFLOW_HIGH_WATER_RATIO, DEFAULT_OVERFLOW_LOW_WATER_RATIO,
    DEFAULT_OVERFLOW_MAX_PUSHES_PER_TICK, DEFAULT_OVERFLOW_TICK_INTERVAL_MS,
};
pub use metrics::{
    evaluate_health_gate, BlobMetrics, BlobMetricsSnapshot, HealthGateAction,
    OverflowMetricsSnapshot, HEALTH_GATE_CLEAR_THRESHOLD, HEALTH_GATE_EMIT_THRESHOLD,
};
pub use migration::{
    drive_blob_migration_tick, drive_blob_migration_tick_arc,
    drive_blob_migration_tick_with_manifest_resolver, parse_blob_heat_tag, BlobMigrationCandidate,
    BlobMigrationController, BlobMigrationTickReport, ManifestSiblings,
};
pub use noop::NoopAdapter;
pub use overflow::{
    drive_blob_overflow_tick, step_overflow_hysteresis, BlobOverflowCandidate,
    BlobOverflowController, BlobOverflowTickReport, OverflowCandidateBatch, OverflowPushSink,
    OverflowTickContext, OverflowTickObservation,
};
pub use publish_with_blob::{publish_with_blob, BlobDurability, PublishWithBlobReceipt};
pub use refcount::{should_sweep, BlobRefcountTable, RefcountEntry, DEFAULT_RETENTION_FLOOR};
pub use registry::{global_blob_adapter_registry, BlobAdapterRegistry, BlobAdapterRegistryError};

#[cfg(test)]
mod hex32_tests {
    use super::*;

    /// Pin dataforts perf #171: the table-lookup `hex32` produces
    /// the same lowercase-hex output as the legacy
    /// `write!("{:02x}", b)` loop, byte-for-byte. A regression
    /// that swapped the nibble order (`b & 0xf` vs `b >> 4`),
    /// switched case, or off-by-oned the index into `HEX_LOWER`
    /// would silently corrupt every channel name, URI, and
    /// operator log line the blob layer emits.
    #[test]
    fn hex32_matches_write_macro_output_byte_for_byte() {
        // Patterns chosen to exercise every nibble value AND the
        // boundary cases (all-zero, all-one, ascending/descending).
        let cases: [[u8; 32]; 4] = [
            [0x00; 32],
            [0xFF; 32],
            {
                let mut a = [0u8; 32];
                for (i, b) in a.iter_mut().enumerate() {
                    *b = i as u8;
                }
                a
            },
            [
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
                0x32, 0x10, 0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe, 0xf0, 0x0d, 0xfa, 0xce,
                0x1b, 0xad, 0xd0, 0x0d,
            ],
        ];

        for hash in &cases {
            // Legacy `write!` loop produces the canonical output
            // we're matching against.
            let mut legacy = String::with_capacity(64);
            for b in hash {
                use std::fmt::Write as _;
                let _ = write!(legacy, "{:02x}", b);
            }
            let modern = hex32(hash);
            assert_eq!(
                modern, legacy,
                "table-lookup hex32 must match write! output for {hash:?}",
            );
            assert_eq!(modern.len(), 64);
            // All bytes must be ASCII lowercase hex.
            for b in modern.as_bytes() {
                assert!(b.is_ascii_hexdigit() && (*b).is_ascii_lowercase() || b.is_ascii_digit());
            }
        }
    }

    /// Pin: `hex32_into` writes to the caller-owned buffer with
    /// the same byte sequence `hex32` produces. The two APIs
    /// share the lookup table; this test catches a refactor that
    /// drifts one out of sync with the other.
    #[test]
    fn hex32_into_matches_hex32_output() {
        let hash: [u8; 32] = [
            0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe, 0xfe, 0xed, 0xfa, 0xce, 0xf0, 0x0d,
            0x1b, 0xad, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x00, 0x11, 0x22, 0x33,
            0x44, 0x55, 0x66, 0x77,
        ];
        let mut buf = [0u8; 64];
        hex32_into(&hash, &mut buf);
        assert_eq!(std::str::from_utf8(&buf).unwrap(), hex32(&hash));
    }
}
