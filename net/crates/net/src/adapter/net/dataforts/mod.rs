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
    AdmitRejectReason, ColocationPolicy, DispatchOutcome, EvictionSweep, GreedyCacheEntry,
    GreedyCacheRegistry, GreedyChannelMetrics, GreedyChannelMetricsAtomic, GreedyClusterMetrics,
    GreedyClusterMetricsAtomic, GreedyConfig, GreedyConfigError, GreedyMetricsRegistry,
    GreedyMetricsSnapshot, GreedyObserver, GreedyRuntime, IntentMatchPolicy, ScopeLabel,
};

#[cfg(feature = "dataforts")]
pub use gravity::{
    should_emit_heat, DataGravityPolicy, DataGravityPolicyError, EmissionDecision, HeatCounter,
    HeatEmission, HeatRegistry, HeatSink,
};

#[cfg(feature = "dataforts")]
pub use blob::{
    classify_payload, global_blob_adapter_registry, publish_blob, publish_blob_ref,
    resolve_payload, run_conformance_suite, BlobAdapter, BlobAdapterRegistry,
    BlobAdapterRegistryError, BlobError, BlobRef, EventPayload, FileSystemAdapter, NoopAdapter,
    BLOB_REF_DISCRIMINATOR, BLOB_REF_VERSION_V1,
};
