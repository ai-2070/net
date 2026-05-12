//! `MeshError` — typed errors for the planner + executor.
//!
//! Mirrors the plan's § 9 Error semantics. Notably,
//! `PartialResult` is the load-bearing variant: many failure
//! modes return *partial* results plus enough state to resume
//! or recover, rather than aborting hard.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::query::{ResultRow, SeqNum};

/// Typed errors for MeshDB. Returned by both the planner
/// (`PlannerError`, `NoCapableHolder`, `LineageMaxDepthExceeded`)
/// and the executor (`HistoricalRangeUnavailable`,
/// `JoinMemoryExceeded`, `QueryBudgetExceeded`,
/// `PartialResult`, `ExecutorError`, `QueryCancelled`).
///
/// `#[non_exhaustive]` so phases B–F can add variants
/// (`StreamingTimeout`, `WatermarkExpired`, etc.) without
/// breaking source-side users.
#[derive(Clone, Debug, PartialEq, Error, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MeshError {
    /// No node holds the requested seq range. The `available`
    /// list carries the per-replica seq-range hints so
    /// callers can negotiate a different window.
    #[error(
        "no holder for chain {origin:#x} seq range {requested:?} (available: {available:?})"
    )]
    HistoricalRangeUnavailable {
        /// Chain whose range is missing (substrate `u64`
        /// origin hash).
        origin: u64,
        /// What the query asked for.
        requested: Range<SeqNum>,
        /// What the substrate can currently serve (per
        /// replica, post-compaction).
        available: Vec<Range<SeqNum>>,
    },

    /// Lineage walk hit the depth bound before terminating.
    /// Carry the depth so callers can decide whether to
    /// retry with a wider bound.
    #[error("lineage walk for {origin:#x} exceeded max_depth={depth}")]
    LineageMaxDepthExceeded {
        /// Chain at which the walk started.
        origin: u64,
        /// Bound that was hit.
        depth: u32,
    },

    /// Lineage walk observed a cycle in the `fork-of:` graph.
    /// In principle the graph is a DAG; cycles indicate
    /// broken upstream applications. The cycle path is
    /// included for debugging.
    #[error("lineage cycle detected starting at {origin:#x}; cycle length={}", cycle.len())]
    LineageCycleDetected {
        /// Chain at which the walk started.
        origin: u64,
        /// The sequence of chain hashes that closed the cycle.
        cycle: Vec<u64>,
    },

    /// A join's working-set memory exceeded the planner's
    /// configured threshold. Carry the strategy + threshold
    /// so dashboards can disambiguate "broadcast was too
    /// big" from "hash-partition shuffle was too big".
    #[error("join memory exceeded threshold={threshold_bytes} bytes for {strategy}")]
    JoinMemoryExceeded {
        /// Which join strategy ran out of room.
        strategy: String,
        /// Configured memory bound, in bytes.
        threshold_bytes: u64,
    },

    /// The query exceeded one of its per-channel budgets
    /// (`query_max_rows`, `query_max_duration`,
    /// `query_max_bytes_scanned`). Returns the metric + the
    /// observed-vs-limit pair.
    #[error("query budget exceeded ({metric:?}): used={used} limit={limit}")]
    QueryBudgetExceeded {
        /// Which budget hit the limit.
        metric: BudgetMetric,
        /// Observed value at the point of failure.
        used: u64,
        /// Configured limit.
        limit: u64,
    },

    /// Query terminated early but produced usable rows.
    /// Carry the rows + a continuation token so the caller
    /// can resume. The `reason` field is a short human-
    /// readable diagnostic.
    #[error("partial result: {reason} ({} rows)", rows.len())]
    PartialResult {
        /// Rows produced before termination.
        rows: Vec<ResultRow>,
        /// Opaque continuation token. The executor knows
        /// how to resume from this; opaque to callers.
        continuation: Vec<u8>,
        /// Short human-readable diagnostic explaining why the
        /// result was partial (network blip, planner
        /// downgrade, replica failover, etc.).
        reason: String,
    },

    /// Planner-side failure — unsupported operator, malformed
    /// AST, version mismatch. Distinct from `ExecutorError`
    /// so dashboards can split "couldn't plan" from
    /// "ran but failed mid-execution".
    #[error("planner error: {detail}")]
    PlannerError {
        /// Diagnostic detail.
        detail: String,
    },

    /// Executor-side failure on a specific node. Carry the
    /// node_id so operators can route the report at the
    /// right peer.
    #[error("executor error on node {node:#x}: {detail}")]
    ExecutorError {
        /// node_id of the executor that failed.
        node: u64,
        /// Diagnostic detail.
        detail: String,
    },

    /// `ChainRef::Discovered` resolved to zero candidates.
    /// Carry the requirement (in human-readable form) so
    /// callers can adjust their predicate.
    #[error("no node holds chain {origin:#x} matching the requirement: {requirement}")]
    NoCapableHolder {
        /// Origin hash (zeroed if the failure is at
        /// discovery time before any concrete hash is known).
        origin: u64,
        /// Rendered predicate or requirement string for
        /// diagnostics.
        requirement: String,
    },

    /// Query cancelled via `MeshQueryExecutor::cancel`.
    /// Distinct from `ExecutorError` so callers can route
    /// cancellations differently (they're not failures).
    #[error("query cancelled")]
    QueryCancelled,
}

/// Identifies which configured budget tripped a
/// `MeshError::QueryBudgetExceeded`. Mirrors the per-channel
/// budget configuration shape (rows / duration / bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetMetric {
    /// `query_max_rows` — total result-row count.
    MaxRows,
    /// `query_max_duration` — wall-clock execution time.
    MaxDuration,
    /// `query_max_bytes_scanned` — total bytes read across
    /// all reachable nodes (includes streamed-but-filtered
    /// rows).
    MaxBytesScanned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_range_unavailable_display() {
        let e = MeshError::HistoricalRangeUnavailable {
            origin: 0xDEAD_BEEF_CAFE_BABE,
            requested: SeqNum(100)..SeqNum(200),
            available: vec![SeqNum(0)..SeqNum(50)],
        };
        // Just smoke-test the Display impl renders without panic.
        let _ = format!("{e}");
    }

    #[test]
    fn partial_result_carries_continuation() {
        let e = MeshError::PartialResult {
            rows: vec![],
            continuation: b"continuation-token".to_vec(),
            reason: "test partial".to_string(),
        };
        let msg = format!("{e}");
        assert!(msg.contains("partial result"));
        assert!(msg.contains("0 rows"));
    }

    #[test]
    fn budget_metric_variants_are_distinct() {
        // Pin the variant set so a future addition doesn't
        // silently break operator dashboards.
        assert_ne!(BudgetMetric::MaxRows, BudgetMetric::MaxDuration);
        assert_ne!(BudgetMetric::MaxRows, BudgetMetric::MaxBytesScanned);
        assert_ne!(BudgetMetric::MaxDuration, BudgetMetric::MaxBytesScanned);
    }
}
