//! Greedy-LRU dataforts (Rebel Yell Phase 1).
//!
//! A node observes streams flowing past via the tail-subscription
//! path. If it has spare disk, ACL access, a scope match, the
//! capability set to fulfill the chain's `metadata.intent`, and an
//! optional colocation hint matches a chain it already holds, it
//! caches a copy. When disk fills, LRU evicts. Withdraws the
//! `causal:` capability tag on full eviction so reads route
//! elsewhere.
//!
//! Pure-logic pieces (config validation + admission decision) live
//! here. The async runtime, per-channel cache files, bandwidth
//! budget enforcement, chain announce/withdraw wiring, and
//! metrics surface land in subsequent slices — this slice is the
//! decision substrate the runtime composes against.
//!
//! Locked design decisions live in `docs/misc/DATAFORTS_PLAN.md`
//! § Phase 1 — Greedy-LRU dataforts — locked decisions.

mod admission;
mod cache;
mod config;
mod metrics;
mod runtime;

pub use admission::{should_admit, AdmissionInputs, AdmissionVerdict};
pub use cache::{EvictedEntry, EvictionSweep, GreedyCacheEntry, GreedyCacheRegistry};
pub use config::{
    GreedyConfig, GreedyConfigError, DEFAULT_BANDWIDTH_BUDGET_FRACTION,
    DEFAULT_PER_CHANNEL_CAP_BYTES, DEFAULT_PROXIMITY_MAX_RTT_MS, DEFAULT_TOTAL_CAP_BYTES,
    MIN_PER_CHANNEL_CAP_BYTES,
};
pub use metrics::{
    AdmitRejectReason, GreedyChannelMetrics, GreedyChannelMetricsAtomic, GreedyClusterMetrics,
    GreedyClusterMetricsAtomic, GreedyMetricsRegistry, GreedyMetricsSnapshot,
    MAX_TRACKED_CHANNELS as MAX_METRIC_TRACKED_CHANNELS, OVERFLOW_CHANNEL_LABEL,
};
pub use runtime::{synthesize_cache_channel_name, DispatchOutcome, GreedyObserver, GreedyRuntime};

// Re-exported from the substrate so callers can compose against a
// single `dataforts::greedy::` import root rather than reaching
// into `behavior::placement` for the policy enums.
pub use crate::adapter::net::behavior::placement::{
    ColocationPolicy, IntentMatchPolicy, ScopeLabel,
};
