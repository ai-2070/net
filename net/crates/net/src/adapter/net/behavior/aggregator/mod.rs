//! Aggregator daemon — bridges subnet tiers by summarizing
//! detail-fold state and republishing the summaries on channels
//! with broader visibility.
//!
//! Design + rationale: `docs/plans/SCALING_SUBNET_SPEC.md`
//! Phases B + C. The async lifecycle sibling trait
//! ([`LifecycleDaemon`](crate::adapter::net::behavior::lifecycle::LifecycleDaemon))
//! and its generic group primitive
//! ([`LifecycleGroup`](crate::adapter::net::behavior::lifecycle::LifecycleGroup))
//! live under `behavior::lifecycle` — `AggregatorDaemon` is the
//! first consumer.

pub mod config;
pub mod daemon;
pub mod query_client;
pub mod query_service;
pub mod registry;
pub mod summarizer;

pub use config::AggregatorConfig;
pub use daemon::{AggregatorDaemon, AggregatorError, AggregatorPublishError};
pub use registry::{AggregatorGroupEntry, AggregatorRegistry, AggregatorRegistryError};
pub use query_client::{
    FoldQueryClient, FoldQueryClientError, DEFAULT_QUERY_CACHE_TTL, DEFAULT_QUERY_DEADLINE,
};
pub use query_service::{
    FoldQueryError, FoldQueryHandler, FoldQueryOp, FoldQueryRequest, FoldQueryResponse,
    FOLD_QUERY_SERVICE,
};
pub use summarizer::{
    CapabilityFoldSummarizer, ReservationFoldSummarizer, Summarizer, SummaryAnnouncement,
};
