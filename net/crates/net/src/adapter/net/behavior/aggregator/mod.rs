//! Aggregator daemon — bridges subnet tiers by summarizing
//! detail-fold state and republishing the summaries on channels
//! with broader visibility.
//!
//! Design + rationale: `docs/plans/SCALING_SUBNET_SPEC.md`
//! Phases B + C. The async lifecycle sibling trait
//! ([`LifecycleDaemon`]) is documented in [`lifecycle`].

pub mod config;
pub mod daemon;
pub mod group;
pub mod lifecycle;
pub mod query_client;
pub mod query_service;
pub mod summarizer;

pub use config::AggregatorConfig;
pub use daemon::{AggregatorDaemon, AggregatorError, AggregatorPublishError};
pub use group::AggregatorGroup;
pub use lifecycle::{LifecycleDaemon, LifecycleError, LifecycleHandle};
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
