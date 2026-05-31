//! Dataforts SDK surface — operator-facing re-exports for the
//! mesh-native blob storage adapter.
//!
//! The substrate's `adapter::net::dataforts::*` lives under the
//! `dataforts` feature on the `net` crate. This module pulls
//! the operator-relevant types up to `net_sdk::dataforts::*`
//! so consumers (e.g. the Deck's DATAFORTS tab) don't have to
//! reach through internal module paths.
//!
//! This module is the storage + operator read side. The on-demand
//! cross-peer *movement* surface (blob transfer, directory transfer,
//! the fairscheduler stream-id helpers) lives in
//! [`net_sdk::transport`](crate::transport).
//!
//! Scope of this re-export: the snapshot types operators read
//! (`BlobMetrics`, `BlobMetricsSnapshot`, `OverflowMetricsSnapshot`)
//! plus the health-gate constants + helper. The full blob-adapter
//! surface (`MeshBlobAdapter`, `FileSystemAdapter`, etc.) stays
//! behind the substrate's path because daemon authors who need
//! it should depend on the substrate directly — the SDK's job
//! here is to expose just the read surface a deck / dashboard
//! cares about.

pub use net::adapter::net::dataforts::{
    evaluate_health_gate, publish_blob_ref, BlobAdapter, BlobInventoryEntry, BlobListOptions,
    BlobMetrics, BlobMetricsSnapshot, BlobRef, HealthGateAction, MeshBlobAdapter,
    OverflowMetricsSnapshot, DEFAULT_RETENTION_FLOOR, HEALTH_GATE_CLEAR_THRESHOLD,
    HEALTH_GATE_EMIT_THRESHOLD,
};
// `Redex` is the underlying storage handle a `MeshBlobAdapter`
// is constructed against. Consumers wiring an adapter need
// the constructor; the rest of the Redex API stays substrate-
// internal.
pub use net::adapter::net::redex::Redex;
