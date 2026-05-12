//! Dataforts — the Rebel Yell compositional layer above the
//! Warriors substrate primitives.
//!
//! Each Dataforts phase ships behind its own Cargo feature flag
//! and composes against the substrate's tag taxonomy, capability
//! index, replication runtime, and placement filter. See
//! `docs/misc/DATAFORTS_PLAN.md` for the activation gates per
//! phase and the locked design decisions per remaining phase.
//!
//! Currently exported phases:
//!
//! - [`greedy`] — per-node speculative caching of in-scope chains
//!   observed via the tail-subscription path. Phase 1.

#[cfg(feature = "dataforts")]
pub mod blob;

#[cfg(feature = "dataforts")]
pub mod greedy;

#[cfg(feature = "dataforts")]
pub mod gravity;

#[cfg(feature = "dataforts")]
pub use greedy::{
    should_admit, synthesize_cache_channel_name, AdmissionInputs, AdmissionVerdict,
    AdmitRejectReason, ColocationPolicy, DispatchOutcome, EvictedEntry, EvictionSweep,
    GreedyCacheEntry, GreedyCacheRegistry, GreedyChannelMetrics, GreedyChannelMetricsAtomic,
    GreedyClusterMetrics, GreedyClusterMetricsAtomic, GreedyConfig, GreedyConfigError,
    GreedyMetricsRegistry, GreedyMetricsSnapshot, GreedyObserver, GreedyRuntime, IntentMatchPolicy,
    ScopeLabel,
};

#[cfg(feature = "dataforts")]
pub use gravity::{
    should_emit_heat, BlobHeatRegistry, DataGravityPolicy, DataGravityPolicyError,
    EmissionDecision, HeatCounter, HeatEmission, HeatRegistry, HeatSink,
};

#[cfg(feature = "dataforts")]
pub use blob::{
    auth_allows_blob_op, byte_range_to_chunks, chunk_payload, classify_payload,
    evaluate_health_gate, global_blob_adapter_registry, publish_blob, publish_blob_ref,
    publish_with_blob, resolve_payload, run_conformance_suite, should_migrate_blob_to,
    should_pull_blob, should_sweep, BlobAdapter, BlobAdapterRegistry, BlobAdapterRegistryError,
    BlobDurability, BlobError, BlobMetrics, BlobMetricsSnapshot, BlobRef, BlobRefcountTable,
    BlobStat, ChunkRangeRequest, ChunkRef, ChunkedPayload, Encoding, EventPayload,
    FileSystemAdapter, HealthGateAction, MeshBlobAdapter, MigrateBlobReject, MigrateBlobVerdict,
    NoopAdapter, PublishWithBlobReceipt, PullBlobReject, PullBlobVerdict, RefcountEntry,
    BLOB_CHUNK_SIZE_BYTES, BLOB_MANIFEST_BODY_VERSION, BLOB_MANIFEST_MAX_CHUNKS,
    BLOB_REF_DISCRIMINATOR, BLOB_REF_MAGIC, BLOB_REF_MAX_SIZE, BLOB_REF_SMALL_HEADER_LEN,
    BLOB_REF_VERSION_V1, BLOB_REF_VERSION_V2_MANIFEST, DEFAULT_BLOB_HEAT_HALF_LIFE,
    DEFAULT_RETENTION_FLOOR, HEALTH_GATE_CLEAR_THRESHOLD, HEALTH_GATE_EMIT_THRESHOLD,
};
