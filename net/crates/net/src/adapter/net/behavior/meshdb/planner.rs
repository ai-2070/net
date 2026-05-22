//! `MeshQueryPlanner` — translates a [`MeshQuery`] AST into an
//! [`ExecutionPlan`] tree the executor walks at run time.
//!
//! Phase A scope: atomic operators (`At` / `Between` / `Latest`)
//! plan completely; composite operators (`Join`, `Filter`,
//! `Aggregate`, `Project`, `OrderBy`) recurse into their inner
//! sub-queries, plan the inner, and surface
//! `OperatorPlan::NotYetImplemented` until their phase activates.
//! Phase C extended `LineageBack` / `LineageForward` to fully-
//! planned leaf operators via [`OperatorPlan::LineageEmit`] —
//! the walk runs at plan time against the capability-index
//! snapshot, and the executor emits one [`super::query::ResultRow`]
//! per entry.
//!
//! # Determinism contract
//!
//! Per the plan, the planner is a pure function: same query +
//! same capability-index state produces the same plan. This is
//! load-bearing for the result cache (locked decision #4 keys
//! on `(query_hash, capability_index_version)`).
//!
//! Phase A's planner is deterministic by construction — every
//! lookup orders its results lexicographically by node_id, and
//! the cost-model stub never depends on iteration order. Phases
//! B–F preserve this contract.
//!
//! # Cost model (Phase A stub)
//!
//! Each plan node carries a [`CostEstimate`] (with
//! `bandwidth_bytes` and `latency_ms` fields). Phase A
//! populates these conservatively: every atomic-operator node
//! uses a proximity-based latency from the capability index's
//! RTT graph (or `0` if unknown), and a bandwidth heuristic of
//! `64 KiB` per node (chain reads are typically small). Phase
//! B replaces the bandwidth heuristic with cardinality
//! estimates pulled via [`CapabilityQuery::aggregate`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::error::MeshError;
use super::query::{AggregateFn, ChainRef, Expr, JoinKey, JoinKind, MeshQuery, QueryV1, SeqNum};
use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
use crate::adapter::net::behavior::predicate::PredicateWire;
use crate::adapter::net::behavior::query::CapabilityQuery;
use crate::adapter::net::behavior::tag::{Tag, TaxonomyAxis};

/// A planned-but-not-yet-executed query tree. Each node is
/// annotated with execution metadata (target nodes, capability
/// requirements, cost estimate, result schema). The executor
/// walks this tree.
///
/// Carries `Serialize + Deserialize` so plans can ride the wire
/// to remote executors (Phase B's federation layer + the cache
/// invalidation key both consume the encoded form).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// Top-level operator node. The tree is acyclic: composite
    /// operators reference children via [`OperatorNode`]'s
    /// `inputs` field.
    pub root: OperatorNode,
    /// Total estimated cost summed across all nodes in the
    /// tree. Operators on the cost-driven path (join strategy
    /// selection in Phase D) consult this.
    pub total_cost: CostEstimate,
}

/// One node in the [`ExecutionPlan`] tree. Carries the operator
/// shape + the executor-targeting metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperatorNode {
    /// The operator + its operator-specific parameters.
    pub operator: OperatorPlan,
    /// `node_id`s that hold the data this operator reads.
    /// Empty for nodes that don't touch the substrate
    /// directly (e.g. a top-of-tree `Project` that runs at
    /// the caller). Ordered lexicographically for
    /// determinism.
    pub target_nodes: Vec<u64>,
    /// Cost estimate for this operator alone (not the
    /// subtree). Phase A populates conservatively; phases
    /// B–E refine.
    pub cost: CostEstimate,
}

/// Operator-specific shape inside an [`OperatorNode`]. Mirrors
/// the [`QueryV1`] variants with planner annotations baked in.
///
/// Composite operators (`Filter`, `Aggregate`, `Project`,
/// `OrderBy`, `Join`, `Lineage*`) carry their inputs as
/// `Box<OperatorNode>` so the tree is fully typed. Phase A
/// ships the operator-plan shape; Phases B–E populate it for
/// every variant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum OperatorPlan {
    /// Read a single event at `seq` from one of the
    /// `target_nodes`.
    AtRead {
        /// Resolved chain origin hash (post-`ChainRef::Discovered`
        /// resolution; substrate `u64`).
        origin: u64,
        /// Sequence number to read.
        seq: SeqNum,
    },
    /// Read events in `[start, end)` from one of the
    /// `target_nodes`. Half-open range.
    BetweenRead {
        /// Resolved chain origin hash.
        origin: u64,
        /// Lower bound (inclusive).
        start: SeqNum,
        /// Upper bound (exclusive).
        end: SeqNum,
    },
    /// Read the tip event from one of the `target_nodes`.
    LatestRead {
        /// Resolved chain origin hash.
        origin: u64,
    },
    /// Composite — Filter inner rows by predicate. Phase E
    /// territory; the planner surface lands now so downstream
    /// code can pattern-match against the variant.
    Filter {
        /// Inner sub-plan.
        input: Box<OperatorNode>,
        /// Filter predicate (wire form).
        predicate: PredicateWire,
    },
    /// Materialized lineage walk — the planner snapshotted the
    /// `fork-of:` graph (backward or forward) and produced
    /// `entries` in walk order. The executor emits one
    /// [`ResultRow`] per entry: `origin = entry.origin`,
    /// `seq = entry.tip_seq.unwrap_or(SeqNum(0))`, payload
    /// empty. Callers wanting full event content compose with
    /// `At` / `Between` against each entry's origin.
    ///
    /// Walk-at-plan-time uses the local capability index as a
    /// snapshot, matching the plan's "no lineage streaming"
    /// scope for Phase C. Drift between snapshot + read time
    /// is bounded by the CAP-ANN broadcast cadence.
    ///
    /// [`ResultRow`]: super::query::ResultRow
    LineageEmit {
        /// Start origin of the walk (post-`ChainRef::Discovered`
        /// resolution).
        origin: u64,
        /// Walk direction.
        direction: LineageDirection,
        /// One entry per chain reached. Ordered: ancestors-
        /// first for `Back`, BFS-asc-depth for `Forward`. Always
        /// includes the start origin at index 0 with `depth = 0`.
        entries: Vec<LineageEntry>,
    },
    /// Phase E-1 count aggregate. Executor collects the inner
    /// sub-plan's rows, groups by the row-intrinsic key (or
    /// uses a single bucket when `group_by` is `None`), and
    /// emits one [`super::query::ResultRow`] per group whose
    /// `payload` is a postcard-encoded
    /// [`super::query::AggregateRowPayload`] carrying the
    /// group identifier + count.
    AggregateCount {
        /// Inner sub-plan whose rows are counted.
        input: Box<OperatorNode>,
        /// Row-intrinsic group key. `None` = single bucket
        /// (total count). Phase E-1 supports only the row-
        /// intrinsic modes; payload-keyed grouping lands in
        /// Phase E-2 alongside row-schema decoding.
        group_by: Option<JoinKeyMode>,
    },
    /// Phase E-3 numeric aggregate (Sum / Avg) over a
    /// row-intrinsic or JSON-payload field. The executor
    /// extracts a `f64` per row via
    /// [`super::row::extract_numeric`], skips rows whose
    /// extraction returns `None`, and emits one
    /// [`super::query::ResultRow`] per group carrying a
    /// postcard-encoded
    /// [`super::query::AggregateRowPayload`].
    AggregateNumeric {
        /// Inner sub-plan whose rows are aggregated.
        input: Box<OperatorNode>,
        /// Row-intrinsic group key, or `None` for a single
        /// bucket.
        group_by: Option<JoinKeyMode>,
        /// Field path to extract the numeric value from. See
        /// [`super::row::extract_numeric`] for resolution.
        field_path: String,
        /// Which numeric function to apply.
        kind: super::query::NumericAggregateKind,
    },
    /// Phase E-4 numeric reduction (Min / Max / exact
    /// Percentile). Collects every per-row numeric value into
    /// the group, then reduces. `Percentile` sorts the bag and
    /// picks the nearest-rank quantile.
    AggregateReduction {
        /// Inner sub-plan whose rows are reduced.
        input: Box<OperatorNode>,
        /// Row-intrinsic group key.
        group_by: Option<JoinKeyMode>,
        /// Field path.
        field_path: String,
        /// Reduction kind.
        kind: super::query::NumericReductionKind,
    },
    /// Phase E-4 exact distinct count over a row-intrinsic /
    /// JSON field. Tracks the canonical string form of each
    /// leaf value in a per-group `BTreeSet`. Bounded by the
    /// executor's per-query memory budget.
    AggregateDistinct {
        /// Inner sub-plan whose rows are aggregated.
        input: Box<OperatorNode>,
        /// Row-intrinsic group key.
        group_by: Option<JoinKeyMode>,
        /// Field path. The leaf value's canonical string form
        /// is what gets hashed for distinctness.
        field_path: String,
    },
    /// Phase E-5 tumbling window operator. The executor reads
    /// `input`'s rows, buckets them by
    /// [`super::query::WindowSpec`], and emits one sentinel
    /// [`super::query::ResultRow`] per non-empty bucket whose
    /// `payload` is a postcard-encoded
    /// [`super::query::WindowBoundary`].
    Window {
        /// Inner sub-plan whose rows are bucketed.
        input: Box<OperatorNode>,
        /// Window strategy.
        spec: super::query::WindowSpec,
    },
    /// Inner hash-join of two sub-plans. Phase D-1 ships the
    /// in-memory hash-build-on-left / probe-on-right strategy
    /// against row-intrinsic keys (`origin` / `seq`); richer
    /// key extraction over event payloads lands in Phase E
    /// once row schemas are decoded.
    ///
    /// The executor emits one [`super::query::ResultRow`] per
    /// matched pair: `origin = 0` (sentinel), `seq = SeqNum(0)`
    /// (sentinel), `payload =` postcard-encoded
    /// [`super::query::JoinedRowPayload`] carrying the
    /// original `(left, right)` rows.
    HashJoin {
        /// Left sub-plan.
        left: Box<OperatorNode>,
        /// Right sub-plan.
        right: Box<OperatorNode>,
        /// How to extract the join key from each side's rows.
        key_mode: JoinKeyMode,
        /// Inner / outer semantics. All four kinds ship in
        /// Phase D-2.
        kind: JoinKind,
        /// Algorithm picker. Phase D-1 shipped
        /// `HashBroadcast`; Phase D-2 adds `SortMerge` for
        /// the unbounded-build-side case.
        strategy: JoinStrategy,
        /// Late-arrival watermark per locked decision #2.
        /// MeshDB's executor runs over snapshot inputs, so
        /// the watermark is informational only at present —
        /// streaming-window joins are a Phase F+ extension
        /// once a consumer drives the streaming semantics.
        watermark: Duration,
    },
    /// Placeholder operator — emitted by the planner when an
    /// operator's executor hasn't been wired yet. Carries a
    /// diagnostic so the executor can surface a useful
    /// `MeshError::PlannerError` at run time, and the inner
    /// sub-plan so the rest of the tree still type-checks /
    /// optimizes / tests.
    NotYetImplemented {
        /// Diagnostic shown to the operator (e.g. "Join not
        /// yet implemented in Phase A").
        detail: String,
        /// Inner sub-plan (None for atomic operators, Some
        /// for composites whose inner already planned).
        input: Option<Box<OperatorNode>>,
    },
}

/// Join-key extraction mode. Started in Phase D-1 with row-
/// intrinsic-only modes; Phase D-2 adds payload-keyed extraction
/// via [`JoinKeyMode::Field`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinKeyMode {
    /// Hash on `ResultRow.origin` (8-byte LE encoding).
    Origin,
    /// Hash on `ResultRow.seq.0` (8-byte LE encoding).
    Seq,
    /// Hash on the `(origin, seq)` tuple (16-byte LE encoding).
    OriginSeq,
    /// Hash on the canonical string projection of a row's
    /// payload field at `path`. JSON payloads only; rows whose
    /// path resolves to a missing key, non-JSON payload, or
    /// non-scalar leaf are silently dropped from the build
    /// side (so unmatched-build outer-join semantics treat
    /// them as if the row never existed).
    Field(String),
}

/// Hash-join algorithm picker. Phase D-1 shipped broadcast
/// hash; Phase D-2 adds sort-merge as an alternative for very
/// large inputs whose build side wouldn't fit in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinStrategy {
    /// In-memory hash table on the build side, probe on the
    /// other side. Best for small-or-medium build sides;
    /// surfaces [`super::error::MeshError::JoinMemoryExceeded`]
    /// past the configured bound.
    HashBroadcast,
    /// Sort both sides on the join key, then two-pointer
    /// merge. Better for very large inputs that won't fit the
    /// hash table's memory bound. Phase D-2's planner picks
    /// hash by default; consumers driving the choice point at
    /// sort-merge via the explicit operator.
    SortMerge,
}

/// Direction of a [`OperatorPlan::LineageEmit`] walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineageDirection {
    /// Walk `fork-of:` parents toward ancestors. The
    /// `CapabilityIndex` answer is direct: each chain's
    /// `fork-of:<parent_hash>` tag names its parent.
    Back,
    /// Walk `fork-of:` descendants. Scans every entry in the
    /// capability index for a `fork-of:<this_origin>` tag,
    /// BFS-style, sorted by chain hash for determinism.
    Forward,
}

/// One chain reached during a [`OperatorPlan::LineageEmit`] walk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineageEntry {
    /// Chain origin hash (substrate `u64`).
    pub origin: u64,
    /// Hops from the walk's start. `0` for the start origin.
    pub depth: u32,
    /// Best-known tip seq from the holders' `causal:` claims.
    /// `None` when no holder advertises a `Tip` or `Range`
    /// claim (e.g. presence-only).
    pub tip_seq: Option<SeqNum>,
}

/// Planner cost estimate. Phase A uses a conservative
/// proximity + heuristic-bandwidth stub; later phases refine
/// against the capability index's `aggregate` primitive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    /// Estimated bytes the operator will read / produce.
    pub bandwidth_bytes: u64,
    /// Estimated latency in milliseconds. Phase A uses the
    /// proximity-graph RTT to the target node.
    pub latency_ms: u64,
}

/// Heuristic bandwidth per atomic read in Phase A. Chain reads
/// are typically small; refining this requires cardinality
/// estimates that ship in Phase B.
const PHASE_A_ATOMIC_BANDWIDTH_BYTES: u64 = 64 * 1024;

/// The planner. Borrows the capability index for holder
/// lookups + `ChainRef::Discovered` resolution. Borrows an RTT
/// lookup closure so the cost-estimate path stays decoupled
/// from the proximity-graph internals (mirrors the
/// `CapabilityQuery::nearest` pattern).
///
/// Lifetimes: `'a` ties the borrows together; one planner per
/// `plan()` call typically. The planner itself holds no state
/// — same inputs produce the same plan.
pub struct MeshQueryPlanner<'a, F>
where
    F: Fn(u64) -> Option<Duration>,
{
    /// Capability fold — read path for all holder discovery
    /// (parent_of / children_of / collect_coverage) and the
    /// `Discovered`-ref predicate filter
    /// ([`capability_bridge::filter_by_predicate`]).
    pub capability_fold: &'a Fold<CapabilityFold>,
    /// RTT lookup closure. Same shape as
    /// `CapabilityQuery::nearest`.
    pub rtt_lookup: F,
}

impl<'a, F> MeshQueryPlanner<'a, F>
where
    F: Fn(u64) -> Option<Duration>,
{
    /// Construct a planner. Doesn't allocate.
    pub fn new(capability_fold: &'a Fold<CapabilityFold>, rtt_lookup: F) -> Self {
        Self {
            capability_fold,
            rtt_lookup,
        }
    }

    /// Translate a [`MeshQuery`] into an [`ExecutionPlan`].
    /// Phase A handles atomic operators end-to-end; composite
    /// operators recurse into their inner sub-queries and
    /// emit a `NotYetImplemented` wrapper for the outer.
    ///
    /// Errors per `MeshError`:
    /// - `PlannerError { detail: "unsupported query version" }`
    ///   if the outer enum version isn't `V1`.
    /// - `NoCapableHolder { ... }` if a `Discovered` predicate
    ///   resolves to zero origin hashes.
    /// - `HistoricalRangeUnavailable { ... }` if a `Between`
    ///   query's range isn't covered by any reachable holder.
    pub fn plan(&self, query: &MeshQuery) -> Result<ExecutionPlan, MeshError> {
        let root = match query {
            MeshQuery::V1(v1) => self.plan_v1(v1)?,
            // Locked decision #1: future versions reject
            // cleanly. The non-exhaustive match below is for
            // forward-compat — adding `V2` lands the same
            // error path with no source-side break.
            #[allow(unreachable_patterns)]
            _ => {
                return Err(MeshError::PlannerError {
                    detail: "unsupported query version".to_string(),
                });
            }
        };
        let total_cost = sum_cost(&root);
        Ok(ExecutionPlan { root, total_cost })
    }

    /// Dispatch a single `QueryV1` variant. Recursive for
    /// composite operators.
    fn plan_v1(&self, q: &QueryV1) -> Result<OperatorNode, MeshError> {
        match q {
            QueryV1::At { origin, seq } => self.plan_at(origin, *seq),
            QueryV1::Between { origin, start, end } => self.plan_between(origin, *start, *end),
            QueryV1::Latest { origin } => self.plan_latest(origin),

            QueryV1::Filter { inner, predicate } => {
                // Phase A: plan the inner; wrap with a Filter
                // operator-plan node. The Filter executor
                // itself lands in Phase E (per the plan).
                let input = self.plan(inner)?;
                let cost = CostEstimate {
                    // Predicate evaluation is local — no
                    // additional bandwidth beyond reading.
                    bandwidth_bytes: 0,
                    latency_ms: 0,
                };
                Ok(OperatorNode {
                    operator: OperatorPlan::Filter {
                        input: Box::new(input.root),
                        predicate: predicate.clone(),
                    },
                    target_nodes: vec![],
                    cost,
                })
            }

            QueryV1::LineageBack { origin, max_depth } => {
                self.plan_lineage(origin, *max_depth, LineageDirection::Back)
            }
            QueryV1::LineageForward { origin, max_depth } => {
                self.plan_lineage(origin, *max_depth, LineageDirection::Forward)
            }
            QueryV1::Join {
                left,
                right,
                on,
                kind,
                watermark,
            } => self.plan_join(left, right, on, *kind, *watermark),
            QueryV1::Aggregate {
                inner,
                group_by,
                agg_fn,
            } => self.plan_aggregate(inner, group_by, agg_fn),
            QueryV1::Project { inner, .. } => {
                let input = self.plan(inner)?;
                self.plan_not_yet_implemented("Project (Phase A.2)", Some(Box::new(input.root)))
            }
            QueryV1::OrderBy { inner, .. } => {
                let input = self.plan(inner)?;
                self.plan_not_yet_implemented("OrderBy (Phase A.2)", Some(Box::new(input.root)))
            }
            QueryV1::Window { inner, spec } => self.plan_window(inner, spec),
        }
    }

    /// Plan a tumbling-window operator. Phase E-5 ships
    /// `WindowSpec::TumblingSeq { size }` with `size >= 1`.
    fn plan_window(
        &self,
        inner: &MeshQuery,
        spec: &super::query::WindowSpec,
    ) -> Result<OperatorNode, MeshError> {
        match spec {
            super::query::WindowSpec::TumblingSeq { size } if *size == 0 => {
                return Err(MeshError::PlannerError {
                    detail: "Window size must be >= 1".to_string(),
                });
            }
            _ => {}
        }
        let inner_plan = self.plan(inner)?;
        let cost = CostEstimate {
            bandwidth_bytes: inner_plan.total_cost.bandwidth_bytes,
            latency_ms: inner_plan.total_cost.latency_ms,
        };
        Ok(OperatorNode {
            operator: OperatorPlan::Window {
                input: Box::new(inner_plan.root),
                spec: spec.clone(),
            },
            target_nodes: vec![],
            cost,
        })
    }

    /// Plan an atomic `At(origin, seq)` read. Resolves the
    /// origin, then picks holders whose advertised
    /// `causal:` coverage includes `seq` — either an
    /// inclusive range covering `seq`, or a tip-form holder
    /// whose tip is `>= seq` (tip-form implies the holder
    /// has the full prefix up to that tip), or a presence-
    /// form holder (which makes no range claim and is taken
    /// as a permissive fallback). Targets are ordered by
    /// proximity (RTT-asc; ties broken lex-NodeId).
    ///
    /// Returns `HistoricalRangeUnavailable` when no holder's
    /// advertised coverage includes `seq`, with hints
    /// extracted from every advertised range / tip.
    fn plan_at(&self, origin: &ChainRef, seq: SeqNum) -> Result<OperatorNode, MeshError> {
        let origin_hash = self.resolve_origin(origin)?;
        let coverage = self.collect_coverage(origin_hash);
        let targets = self.select_targets_at(&coverage, seq);
        if targets.is_empty() && !coverage.is_empty() {
            return Err(MeshError::HistoricalRangeUnavailable {
                origin: origin_hash,
                requested: seq..SeqNum(seq.0.saturating_add(1)),
                available: coverage
                    .into_iter()
                    .filter_map(|c| c.claim.advertised())
                    .collect(),
            });
        }
        let cost = self.atomic_cost(&targets);
        Ok(OperatorNode {
            operator: OperatorPlan::AtRead {
                origin: origin_hash,
                seq,
            },
            target_nodes: targets,
            cost,
        })
    }

    /// Plan an atomic `Between(origin, start, end)` read.
    /// Range gate: `start < end` (otherwise typed
    /// `PlannerError`). Holder selection requires coverage
    /// of the full requested range:
    ///
    /// - A tip-form holder (`causal:<hex>:<tip_seq>`) covers
    ///   `[0, tip_seq + 1)`.
    /// - A range-form holder (`causal:<hex>[s..e]`) covers
    ///   `[s, e)` exactly.
    /// - A presence-form holder (`causal:<hex>` bare) is
    ///   admitted permissively — it makes no range claim
    ///   and is treated as best-effort.
    ///
    /// Surfaces `HistoricalRangeUnavailable` when no holder
    /// covers the full requested range, with available-range
    /// hints from every covering / partial holder.
    fn plan_between(
        &self,
        origin: &ChainRef,
        start: SeqNum,
        end: SeqNum,
    ) -> Result<OperatorNode, MeshError> {
        if start >= end {
            return Err(MeshError::PlannerError {
                detail: format!("Between requires start < end; got {start:?} >= {end:?}"),
            });
        }
        let origin_hash = self.resolve_origin(origin)?;
        let coverage = self.collect_coverage(origin_hash);
        let targets = self.select_targets_between(&coverage, start, end);
        if targets.is_empty() && !coverage.is_empty() {
            return Err(MeshError::HistoricalRangeUnavailable {
                origin: origin_hash,
                requested: start..end,
                available: coverage
                    .into_iter()
                    .filter_map(|c| c.claim.advertised())
                    .collect(),
            });
        }
        let cost = self.atomic_cost(&targets);
        Ok(OperatorNode {
            operator: OperatorPlan::BetweenRead {
                origin: origin_hash,
                start,
                end,
            },
            target_nodes: targets,
            cost,
        })
    }

    /// Plan an atomic `Latest(origin)` read. Any holder with
    /// the chain in its capability index qualifies; tip-form
    /// holders are preferred (their tip is the candidate
    /// latest), then range-form (highest end-of-range), then
    /// presence-form (no claim — best-effort fallback).
    fn plan_latest(&self, origin: &ChainRef) -> Result<OperatorNode, MeshError> {
        let origin_hash = self.resolve_origin(origin)?;
        let coverage = self.collect_coverage(origin_hash);
        let targets = self.select_targets_latest(&coverage);
        let cost = self.atomic_cost(&targets);
        Ok(OperatorNode {
            operator: OperatorPlan::LatestRead {
                origin: origin_hash,
            },
            target_nodes: targets,
            cost,
        })
    }

    /// Plan a `LineageBack` / `LineageForward` walk against
    /// the local capability-index snapshot.
    ///
    /// Errors per `MeshError`:
    /// - `LineageCycleDetected` if the walk revisits a chain
    ///   (the `fork-of:` graph should be a DAG; cycles indicate
    ///   broken upstream applications).
    /// - `LineageMaxDepthExceeded` if the walk hits `max_depth`
    ///   with more candidates still queued.
    fn plan_lineage(
        &self,
        origin: &ChainRef,
        max_depth: u32,
        direction: LineageDirection,
    ) -> Result<OperatorNode, MeshError> {
        let origin_hash = self.resolve_origin(origin)?;
        let entries = match direction {
            LineageDirection::Back => self.walk_lineage_back(origin_hash, max_depth)?,
            LineageDirection::Forward => self.walk_lineage_forward(origin_hash, max_depth)?,
        };
        // Lineage cost is a function of how many chains we
        // touch; conservative bandwidth estimate is one
        // ResultRow per entry (small, no payload), zero RTT
        // since the walk already happened at plan time.
        let cost = CostEstimate {
            bandwidth_bytes: entries.len() as u64 * 64,
            latency_ms: 0,
        };
        Ok(OperatorNode {
            operator: OperatorPlan::LineageEmit {
                origin: origin_hash,
                direction,
                entries,
            },
            target_nodes: vec![],
            cost,
        })
    }

    /// Plan a Phase D-1 hash join. Recurses on both sides,
    /// then derives the [`JoinKeyMode`] from the [`JoinKey`].
    /// Surfaces `PlannerError` when the key references
    /// payload fields (deferred to Phase E) or the sides
    /// disagree on which intrinsic field to key on.
    fn plan_join(
        &self,
        left: &MeshQuery,
        right: &MeshQuery,
        on: &JoinKey,
        kind: JoinKind,
        watermark: Duration,
    ) -> Result<OperatorNode, MeshError> {
        let key_mode = key_mode_for_join(on)?;
        let left_plan = self.plan(left)?;
        let right_plan = self.plan(right)?;
        let cost = CostEstimate {
            bandwidth_bytes: left_plan
                .total_cost
                .bandwidth_bytes
                .saturating_add(right_plan.total_cost.bandwidth_bytes),
            latency_ms: left_plan
                .total_cost
                .latency_ms
                .max(right_plan.total_cost.latency_ms),
        };
        // Phase D-2 strategy pick: hash-broadcast by default
        // (matches the original Phase D-1 behaviour). A future
        // refinement consults cardinality estimates from the
        // capability index's `aggregate` primitive — see the
        // plan's Cost Model section.
        let strategy = JoinStrategy::HashBroadcast;
        Ok(OperatorNode {
            operator: OperatorPlan::HashJoin {
                left: Box::new(left_plan.root),
                right: Box::new(right_plan.root),
                key_mode,
                kind,
                strategy,
                watermark,
            },
            target_nodes: vec![],
            cost,
        })
    }

    /// Plan an aggregate. Phase E-1 shipped `Count`; Phase E-3
    /// adds `Sum` and `Avg`. Other aggregate functions (sketches
    /// in Phase E-4) still surface `PlannerError`.
    fn plan_aggregate(
        &self,
        inner: &MeshQuery,
        group_by: &[Expr],
        agg_fn: &AggregateFn,
    ) -> Result<OperatorNode, MeshError> {
        let key_mode = group_by_mode(group_by)?;
        let inner_plan = self.plan(inner)?;
        let cost = CostEstimate {
            bandwidth_bytes: inner_plan.total_cost.bandwidth_bytes,
            latency_ms: inner_plan.total_cost.latency_ms,
        };
        let operator = match agg_fn {
            AggregateFn::Count => OperatorPlan::AggregateCount {
                input: Box::new(inner_plan.root),
                group_by: key_mode,
            },
            AggregateFn::Sum { field } => {
                let path = field_path_required(field, "Sum")?;
                OperatorPlan::AggregateNumeric {
                    input: Box::new(inner_plan.root),
                    group_by: key_mode,
                    field_path: path,
                    kind: super::query::NumericAggregateKind::Sum,
                }
            }
            AggregateFn::Avg { field } => {
                let path = field_path_required(field, "Avg")?;
                OperatorPlan::AggregateNumeric {
                    input: Box::new(inner_plan.root),
                    group_by: key_mode,
                    field_path: path,
                    kind: super::query::NumericAggregateKind::Avg,
                }
            }
            AggregateFn::Min { field } => {
                let path = field_path_required(field, "Min")?;
                OperatorPlan::AggregateReduction {
                    input: Box::new(inner_plan.root),
                    group_by: key_mode,
                    field_path: path,
                    kind: super::query::NumericReductionKind::Min,
                }
            }
            AggregateFn::Max { field } => {
                let path = field_path_required(field, "Max")?;
                OperatorPlan::AggregateReduction {
                    input: Box::new(inner_plan.root),
                    group_by: key_mode,
                    field_path: path,
                    kind: super::query::NumericReductionKind::Max,
                }
            }
            AggregateFn::PercentileExact { field, p } => {
                if !p.is_finite() || !(0.0..=1.0).contains(p) {
                    return Err(MeshError::PlannerError {
                        detail: format!("PercentileExact p must be in [0.0, 1.0], got {p}"),
                    });
                }
                let path = field_path_required(field, "PercentileExact")?;
                OperatorPlan::AggregateReduction {
                    input: Box::new(inner_plan.root),
                    group_by: key_mode,
                    field_path: path,
                    kind: super::query::NumericReductionKind::Percentile { p: *p },
                }
            }
            AggregateFn::DistinctCountExact { field } => {
                let path = field_path_required(field, "DistinctCountExact")?;
                OperatorPlan::AggregateDistinct {
                    input: Box::new(inner_plan.root),
                    group_by: key_mode,
                    field_path: path,
                }
            }
            AggregateFn::DistinctCountHll { .. } | AggregateFn::PercentileTDigest { .. } => {
                return Err(MeshError::PlannerError {
                    detail: format!(
                        "aggregate function {agg_fn:?} requires a sketch implementation (HLL p=14 / T-Digest c=100 per locked decision #3); deferred to Phase F. Use DistinctCountExact / PercentileExact for now."
                    ),
                });
            }
        };
        Ok(OperatorNode {
            operator,
            target_nodes: vec![],
            cost,
        })
    }

    /// Walk `fork-of:` parents backward from `start`. Returns
    /// entries in walk order (start first).
    fn walk_lineage_back(
        &self,
        start: u64,
        max_depth: u32,
    ) -> Result<Vec<LineageEntry>, MeshError> {
        let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
        visited.insert(start);
        let mut entries = vec![LineageEntry {
            origin: start,
            depth: 0,
            tip_seq: self.best_tip(start),
        }];
        let mut current = start;
        for depth in 1..=max_depth {
            let Some(parent) = self.parent_of(current) else {
                return Ok(entries);
            };
            if !visited.insert(parent) {
                // Cycle: parent already on the walk. Compute the
                // path from the first occurrence so the error
                // carries the cycle for debugging.
                let mut cycle: Vec<u64> = entries
                    .iter()
                    .map(|e| e.origin)
                    .skip_while(|o| *o != parent)
                    .collect();
                cycle.push(parent);
                return Err(MeshError::LineageCycleDetected {
                    origin: start,
                    cycle,
                });
            }
            entries.push(LineageEntry {
                origin: parent,
                depth,
                tip_seq: self.best_tip(parent),
            });
            current = parent;
        }
        // Reached max_depth: if a further parent still exists,
        // surface the bound. If the walk genuinely terminates
        // exactly at the boundary, no error.
        //
        // `max_depth == 0` is "just-the-origin" — the caller
        // explicitly didn't ask for a walk, so a present parent
        // is not a bound violation. Treat that case as a
        // successful single-entry result.
        if max_depth > 0 && self.parent_of(current).is_some() {
            return Err(MeshError::LineageMaxDepthExceeded {
                origin: start,
                depth: max_depth,
            });
        }
        Ok(entries)
    }

    /// Walk `fork-of:` descendants forward from `start`. BFS
    /// with descendants sorted lex by chain hash so the result
    /// is deterministic.
    fn walk_lineage_forward(
        &self,
        start: u64,
        max_depth: u32,
    ) -> Result<Vec<LineageEntry>, MeshError> {
        use std::collections::VecDeque;
        let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
        visited.insert(start);
        let mut entries = vec![LineageEntry {
            origin: start,
            depth: 0,
            tip_seq: self.best_tip(start),
        }];
        let mut frontier: VecDeque<(u64, u32)> = VecDeque::new();
        frontier.push_back((start, 0));
        while let Some((current, depth)) = frontier.pop_front() {
            // Compute children once per dequeue — previous
            // implementation did it twice (depth check + walk).
            let mut children = self.children_of(current);
            if depth >= max_depth {
                // `max_depth == 0` is "just-the-origin" — the
                // caller explicitly didn't ask for a walk, so
                // present children don't trip the bound. The
                // start node is the only thing on the frontier
                // at depth=0 in that case.
                if max_depth > 0 && !children.is_empty() {
                    return Err(MeshError::LineageMaxDepthExceeded {
                        origin: start,
                        depth: max_depth,
                    });
                }
                continue;
            }
            children.sort_unstable();
            for child in children {
                if !visited.insert(child) {
                    // In a DAG, no cycles. Defensive: a
                    // multi-parent diamond shows up here as a
                    // revisit, which is benign (we just don't
                    // re-add). Treat this case as silently
                    // pruned, not a cycle.
                    continue;
                }
                entries.push(LineageEntry {
                    origin: child,
                    depth: depth + 1,
                    tip_seq: self.best_tip(child),
                });
                frontier.push_back((child, depth + 1));
            }
        }
        Ok(entries)
    }

    /// Find the parent origin for `child` in the capability
    /// index. Scans every indexed node for the one that
    /// advertises `child` via `causal:<hex>` and reads its
    /// `fork-of:<parent_hex>` tag.
    ///
    /// Returns `None` when no node hosts `child` or no hosting
    /// node carries a fork-of declaration.
    ///
    /// Multi-chain hosts — a node with several `fork-of:` tags
    /// — are a Phase C ambiguity. Same for multi-host
    /// replication — two different nodes might both advertise
    /// `causal:<child>` with different `fork-of:` tags.
    /// `CapabilityIndex::all_nodes` iterates a `DashMap` whose
    /// order is unstable across runs, AND `caps.tags` is a
    /// `HashSet` with the same property. We collect every
    /// `(parent_hash, node_id)` candidate across ALL hosting
    /// nodes, sort it, and pick the smallest. That keeps both
    /// the plan AND the load-bearing cache key (locked
    /// decision #4) byte-stable across runs. Mirrors the
    /// symmetric approach `children_of` already uses.
    fn parent_of(&self, child: u64) -> Option<u64> {
        let mut candidates: Vec<(u64, u64)> = Vec::new();
        self.capability_fold.with_state(|state| {
            let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
            for (_key, entry) in state.entries.iter() {
                let node_id = entry.node_id;
                if !visited.insert(node_id) {
                    continue;
                }
                // Walk every class entry for this publisher; union
                // the tag set to match the legacy semantics where
                // CapabilityIndex.get returned the full per-node
                // CapabilitySet across all classes.
                let mut hosts_child = false;
                let mut fork_candidates: Vec<u64> = Vec::new();
                if let Some(keys) = state.by_node.get(&node_id) {
                    for key in keys {
                        let Some(e) = state.entries.get(key) else {
                            continue;
                        };
                        for tag in &e.payload.tags {
                            if let Some(c) = parse_causal_origin_str(tag) {
                                if c == child {
                                    hosts_child = true;
                                }
                            }
                            if let Some(p) = parse_fork_str(tag) {
                                fork_candidates.push(p);
                            }
                        }
                    }
                }
                if hosts_child {
                    for parent in fork_candidates {
                        candidates.push((parent, node_id));
                    }
                }
            }
        });
        candidates.sort_unstable();
        candidates.first().map(|(parent, _)| *parent)
    }

    /// Find all chains advertising `fork-of:<parent>` — i.e.,
    /// the direct descendants. Scans every node in the
    /// capability index; the result is sorted by caller (BFS
    /// needs deterministic order).
    ///
    /// Multi-chain hosts — a node with several `causal:` tags
    /// — are a Phase C ambiguity. `caps.tags` is a `HashSet`,
    /// so we collect every causal body, sort numerically, and
    /// pick the smallest to keep the plan + cache key
    /// deterministic across runs.
    fn children_of(&self, parent: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let publishers: Vec<u64> = self
            .capability_fold
            .with_state(|state| state.by_node.keys().copied().collect());
        for node_id in publishers {
            let tags = crate::adapter::net::behavior::fold::capability_tags_for(
                self.capability_fold,
                node_id,
            );
            let mut has_fork_to_parent = false;
            let mut causal_candidates: Vec<u64> = Vec::new();
            for tag in &tags {
                if let Some(p) = parse_fork_str(tag) {
                    if p == parent {
                        has_fork_to_parent = true;
                    }
                } else if let Some(origin) = parse_causal_origin_str(tag) {
                    causal_candidates.push(origin);
                }
            }
            if has_fork_to_parent {
                causal_candidates.sort_unstable();
                if let Some(&chain) = causal_candidates.first() {
                    if chain != parent {
                        out.push(chain);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Best-known tip seq for `chain` across its holders. Picks
    /// the highest [`CausalClaim::latest_tip`] across all
    /// holders advertising the chain; `None` if all claims are
    /// presence-only (no tip information).
    fn best_tip(&self, chain: u64) -> Option<SeqNum> {
        self.collect_coverage(chain)
            .into_iter()
            .filter_map(|c| c.claim.latest_tip())
            .max()
    }

    /// Resolve a `ChainRef::OriginHash` or `ChainRef::Discovered`
    /// to a concrete `u64` origin hash.
    ///
    /// For `Discovered`: rebuilds the typed `Predicate` from
    /// the stored `PredicateWire`, calls
    /// [`CapabilityQuery::filter`] on the capability index,
    /// then walks each matched node's `causal:<hex>` tags to
    /// extract origin hashes.
    ///
    /// Errors:
    /// - `PlannerError` if the stored `PredicateWire` fails
    ///   to rebuild as a `Predicate`.
    /// - `NoCapableHolder` if the predicate matches zero
    ///   nodes OR every matched node's caps carry no
    ///   `causal:` tag.
    /// - `AmbiguousDiscovery` if the predicate resolves to
    ///   more than one origin. Until Phase B's fan-out
    ///   lands the planner refuses rather than silently
    ///   truncating to the first match — the caller should
    ///   either tighten the predicate or wait for the
    ///   implicit-Union resolution path.
    #[expect(
        clippy::expect_used,
        reason = "match arm `1` guarantees the iterator yields exactly one element"
    )]
    fn resolve_origin(&self, origin: &ChainRef) -> Result<u64, MeshError> {
        match origin {
            ChainRef::OriginHash(h) => Ok(*h),
            ChainRef::Discovered(wire) => {
                let predicate =
                    wire.clone()
                        .into_predicate()
                        .map_err(|e| MeshError::PlannerError {
                            detail: format!("Discovered predicate rebuild failed: {e:?}"),
                        })?;
                let candidates = capability_bridge::filter_by_predicate(
                    self.capability_fold,
                    &predicate,
                );
                // Walk every matched node's caps + extract
                // origins from `causal:<hex>*` tags. Dedupe +
                // sort lex for determinism (BTreeSet is
                // already iterated in sorted order).
                let mut origins: std::collections::BTreeSet<u64> =
                    std::collections::BTreeSet::new();
                for (_node_id, caps) in &candidates {
                    for tag in &caps.tags {
                        if let Some(hash) = parse_causal_origin(tag) {
                            origins.insert(hash);
                        }
                    }
                }
                match origins.len() {
                    0 => Err(MeshError::NoCapableHolder {
                        origin: 0,
                        requirement: format!("{:?}", predicate),
                    }),
                    1 => Ok(*origins.iter().next().expect("len == 1")),
                    _ => Err(MeshError::AmbiguousDiscovery {
                        matches: origins.into_iter().collect(),
                        requirement: format!("{:?}", predicate),
                    }),
                }
            }
        }
    }

    /// Walk the capability index for every node advertising
    /// `causal:<hex>*` for `origin_hash`. Each match emits one
    /// [`HolderCoverage`] carrying the node_id, its RTT (if
    /// the proximity graph has measured one), and the
    /// advertised range / tip / presence form. Used by the
    /// atomic operators' target-selection paths to narrow
    /// to coverage-satisfying holders.
    ///
    /// Result is sorted in the canonical priority order
    /// (RTT-asc, lex-NodeId tiebreak) so target selection is
    /// deterministic across runs (load-bearing for the
    /// locked-decision-#4 cache key).
    fn collect_coverage(&self, origin_hash: u64) -> Vec<HolderCoverage> {
        let hex = chain_hex(origin_hash);
        let mut out: Vec<HolderCoverage> = Vec::new();
        let publishers: Vec<u64> = self
            .capability_fold
            .with_state(|state| state.by_node.keys().copied().collect());
        for node_id in publishers {
            // Each node may advertise multiple `causal:`
            // variants for the same chain (presence + tip +
            // range during transitions). Pick the most
            // specific one — range > tip > presence — so the
            // planner gets the tightest claim. Tie-break on the
            // claim itself so two Range or two Tip claims on
            // the same node resolve deterministically — the
            // cache key per locked decision #4 needs byte-
            // stable plans.
            let tags = crate::adapter::net::behavior::fold::capability_tags_for(
                self.capability_fold,
                node_id,
            );
            let claim = tags
                .iter()
                .filter_map(|t| parse_causal_claim_str(t, &hex))
                .max_by(|a, b| {
                    specificity_rank(a)
                        .cmp(&specificity_rank(b))
                        .then_with(|| claim_cmp_key(a).cmp(&claim_cmp_key(b)))
                });
            if let Some(claim) = claim {
                out.push(HolderCoverage {
                    node_id,
                    rtt: (self.rtt_lookup)(node_id),
                    claim,
                });
            }
        }
        sort_by_proximity(&mut out);
        out
    }

    /// Select target node_ids for an `At(seq)` query. Walks
    /// the pre-sorted (proximity-first, lex tiebreak)
    /// coverage list and keeps holders whose claim covers
    /// `seq`. Result preserves the priority order.
    fn select_targets_at(&self, coverage: &[HolderCoverage], seq: SeqNum) -> Vec<u64> {
        coverage
            .iter()
            .filter(|c| c.claim.covers_seq(seq))
            .map(|c| c.node_id)
            .collect()
    }

    /// Select target node_ids for a `Between(start, end)`
    /// query. Walks the pre-sorted coverage list and keeps
    /// holders whose claim covers the full `[start, end)`
    /// requested range.
    fn select_targets_between(
        &self,
        coverage: &[HolderCoverage],
        start: SeqNum,
        end: SeqNum,
    ) -> Vec<u64> {
        coverage
            .iter()
            .filter(|c| c.claim.covers_range(start, end))
            .map(|c| c.node_id)
            .collect()
    }

    /// Select target node_ids for a `Latest` query. Any
    /// holder with the chain qualifies — there's no
    /// coverage requirement since "latest" is whatever the
    /// holder has on top. Order: holders advertising the
    /// **highest** known tip first (most-current data); then
    /// remaining holders in proximity order. Within
    /// equal-tip holders, proximity-asc with lex-NodeId
    /// tiebreak (inherited from `coverage`'s pre-sort).
    fn select_targets_latest(&self, coverage: &[HolderCoverage]) -> Vec<u64> {
        let mut with_tip: Vec<&HolderCoverage> = coverage
            .iter()
            .filter(|c| c.claim.latest_tip().is_some())
            .collect();
        // Stable sort so the proximity-sort within
        // equal-tip groups carries through. Descending tip
        // = larger first.
        with_tip.sort_by_key(|c| std::cmp::Reverse(c.claim.latest_tip()));
        let mut out: Vec<u64> = with_tip.iter().map(|c| c.node_id).collect();
        // Append presence-only holders (no tip claim) in the
        // pre-sorted order — they're the best-effort fallback.
        for c in coverage {
            if c.claim.latest_tip().is_none() {
                out.push(c.node_id);
            }
        }
        out
    }

    /// Cost-estimate stub for atomic operators.
    /// Bandwidth: heuristic constant per target node.
    /// Latency: proximity RTT to the nearest target, or
    /// `0` if no RTT data exists for any target.
    fn atomic_cost(&self, targets: &[u64]) -> CostEstimate {
        let bandwidth_bytes = (targets.len() as u64) * PHASE_A_ATOMIC_BANDWIDTH_BYTES;
        let latency_ms = targets
            .iter()
            .filter_map(|nid| (self.rtt_lookup)(*nid))
            .map(|d| d.as_millis() as u64)
            .min()
            .unwrap_or(0);
        CostEstimate {
            bandwidth_bytes,
            latency_ms,
        }
    }

    /// Helper for composite operators whose executor hasn't
    /// landed yet. Wraps the planned inner sub-plan (if any)
    /// in a `NotYetImplemented` operator-plan node so the
    /// tree shape stays consistent.
    fn plan_not_yet_implemented(
        &self,
        detail: &str,
        input: Option<Box<OperatorNode>>,
    ) -> Result<OperatorNode, MeshError> {
        Ok(OperatorNode {
            operator: OperatorPlan::NotYetImplemented {
                detail: detail.to_string(),
                input,
            },
            target_nodes: vec![],
            cost: CostEstimate::default(),
        })
    }
}

/// Sum cost across a subtree. Walks the operator-plan
/// recursively. Used by `plan()` to populate
/// `ExecutionPlan.total_cost`.
fn sum_cost(node: &OperatorNode) -> CostEstimate {
    let mut acc = node.cost;
    match &node.operator {
        OperatorPlan::Filter { input, .. } => {
            let inner = sum_cost(input);
            acc.bandwidth_bytes = acc.bandwidth_bytes.saturating_add(inner.bandwidth_bytes);
            acc.latency_ms = acc.latency_ms.saturating_add(inner.latency_ms);
        }
        OperatorPlan::HashJoin { left, right, .. } => {
            let l = sum_cost(left);
            let r = sum_cost(right);
            // Bandwidth sums (both sides fully materialize);
            // latency is the slower of the two (both fetched
            // concurrently at execute time).
            acc.bandwidth_bytes = acc
                .bandwidth_bytes
                .saturating_add(l.bandwidth_bytes)
                .saturating_add(r.bandwidth_bytes);
            acc.latency_ms = acc
                .latency_ms
                .saturating_add(l.latency_ms.max(r.latency_ms));
        }
        OperatorPlan::AggregateCount { input, .. } => {
            let inner = sum_cost(input);
            acc.bandwidth_bytes = acc.bandwidth_bytes.saturating_add(inner.bandwidth_bytes);
            acc.latency_ms = acc.latency_ms.saturating_add(inner.latency_ms);
        }
        OperatorPlan::AggregateNumeric { input, .. } => {
            let inner = sum_cost(input);
            acc.bandwidth_bytes = acc.bandwidth_bytes.saturating_add(inner.bandwidth_bytes);
            acc.latency_ms = acc.latency_ms.saturating_add(inner.latency_ms);
        }
        OperatorPlan::AggregateReduction { input, .. }
        | OperatorPlan::AggregateDistinct { input, .. }
        | OperatorPlan::Window { input, .. } => {
            let inner = sum_cost(input);
            acc.bandwidth_bytes = acc.bandwidth_bytes.saturating_add(inner.bandwidth_bytes);
            acc.latency_ms = acc.latency_ms.saturating_add(inner.latency_ms);
        }
        OperatorPlan::NotYetImplemented {
            input: Some(input), ..
        } => {
            let inner = sum_cost(input);
            acc.bandwidth_bytes = acc.bandwidth_bytes.saturating_add(inner.bandwidth_bytes);
            acc.latency_ms = acc.latency_ms.saturating_add(inner.latency_ms);
        }
        // Atomic + leaf operators (`LineageEmit` is leaf:
        // walk happens at plan time, no children to sum).
        OperatorPlan::AtRead { .. }
        | OperatorPlan::BetweenRead { .. }
        | OperatorPlan::LatestRead { .. }
        | OperatorPlan::LineageEmit { .. }
        | OperatorPlan::NotYetImplemented { input: None, .. } => {}
    }
    acc
}

/// One holder's seq-coverage claim for an origin. Built by
/// [`parse_causal_claim`] from a single `causal:<hex>*`
/// reserved tag; carried alongside the holder's node_id +
/// RTT inside [`HolderCoverage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CausalClaim {
    /// Bare `causal:<hex>` — no range claim. Permissive
    /// fallback: the holder has the chain in some form,
    /// but doesn't advertise what range.
    Presence,
    /// `causal:<hex>:<tip_seq>` — the holder advertises a
    /// full prefix up to and including `tip_seq`. Covers
    /// `[0, tip_seq + 1)`.
    Tip { tip_seq: SeqNum },
    /// `causal:<hex>[start..end]` — the holder advertises
    /// exactly the half-open range `[start, end)`.
    Range { start: SeqNum, end: SeqNum },
}

impl CausalClaim {
    /// Does this claim cover the requested single `seq`?
    /// `Presence` is permissive (best-effort).
    fn covers_seq(&self, seq: SeqNum) -> bool {
        match self {
            Self::Presence => true,
            Self::Tip { tip_seq } => seq.0 <= tip_seq.0,
            Self::Range { start, end } => seq.0 >= start.0 && seq.0 < end.0,
        }
    }

    /// Does this claim cover the half-open requested range
    /// `[start, end)` in full? `Presence` is permissive.
    fn covers_range(&self, start: SeqNum, end: SeqNum) -> bool {
        match self {
            Self::Presence => true,
            Self::Tip { tip_seq } => end.0 <= tip_seq.0.saturating_add(1),
            Self::Range { start: s, end: e } => s.0 <= start.0 && end.0 <= e.0,
        }
    }

    /// Render the claim as an advertised half-open range
    /// (for `HistoricalRangeUnavailable.available` hints).
    /// `None` for `Presence` (no advertised range).
    fn advertised(&self) -> Option<std::ops::Range<SeqNum>> {
        match self {
            Self::Presence => None,
            Self::Tip { tip_seq } => Some(SeqNum(0)..SeqNum(tip_seq.0.saturating_add(1))),
            Self::Range { start, end } => Some(*start..*end),
        }
    }

    /// Highest seq the claim implies the holder has. `None`
    /// for `Presence` (no claim). Used by `Latest` target
    /// selection to prefer the most-current data.
    fn latest_tip(&self) -> Option<SeqNum> {
        match self {
            Self::Presence => None,
            Self::Tip { tip_seq } => Some(*tip_seq),
            Self::Range { end, .. } => Some(SeqNum(end.0.saturating_sub(1))),
        }
    }
}

/// One node's coverage record for a particular origin —
/// node_id, measured RTT (if any), and the parsed
/// `causal:` claim. Carried in the planner's coverage list.
#[derive(Clone, Debug)]
struct HolderCoverage {
    /// node_id of the holder.
    node_id: u64,
    /// Round-trip-time from the local node to this holder
    /// per the proximity graph. `None` when no
    /// measurement exists yet.
    rtt: Option<Duration>,
    /// What the holder advertises about its coverage.
    claim: CausalClaim,
}

/// Specificity rank for `max_by_key` selection within a
/// single holder's `causal:` tag set. Higher = tighter
/// coverage claim. `Range` > `Tip` > `Presence`.
fn specificity_rank(claim: &CausalClaim) -> u8 {
    match claim {
        CausalClaim::Range { .. } => 2,
        CausalClaim::Tip { .. } => 1,
        CausalClaim::Presence => 0,
    }
}

/// Total-order tie-break key for `CausalClaim`. Used to pick
/// a canonical winner when two claims share the same
/// `specificity_rank` — `caps.tags` is a `HashSet`, so the
/// "natural" iteration order is RNG-dependent. The key is
/// shaped so that within each rank the smaller starting seq
/// (and smaller end on a tie) wins; the variant tag is
/// included only as a final tiebreaker so the function is
/// total across all variants.
fn claim_cmp_key(claim: &CausalClaim) -> (u64, u64, u8) {
    match claim {
        CausalClaim::Range { start, end } => (start.0, end.0, 2),
        CausalClaim::Tip { tip_seq } => (0, tip_seq.0, 1),
        CausalClaim::Presence => (u64::MAX, u64::MAX, 0),
    }
}

/// Sort `coverage` in-place by canonical priority:
/// RTT-asc (closer first), unmeasured-RTT last, lex-NodeId
/// tiebreak. Stable so equal-priority holders stay in
/// node_id order across runs (load-bearing for the locked-
/// decision-#4 cache key).
fn sort_by_proximity(coverage: &mut [HolderCoverage]) {
    coverage.sort_by(|a, b| match (a.rtt, b.rtt) {
        (Some(ra), Some(rb)) => ra.cmp(&rb).then_with(|| a.node_id.cmp(&b.node_id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.node_id.cmp(&b.node_id),
    });
}

/// Lowercase hex digits keyed by nibble value `0..=15`. Per
/// meshdb perf #211 — same lookup-table pattern as the
/// dataforts #171 hex-decode fix, but applied to the encode
/// side of the planner's `causal:<hex>` tag stem.
const HEX_NIBBLES: &[u8; 16] = b"0123456789abcdef";

/// Lowercase 16-char hex of a `u64` origin hash. Mirrors
/// `MeshNode::chain_hex` (which is private); duplicated
/// here so the planner doesn't depend on the mesh layer.
///
/// Per perf #211 — pre-fix `format!("{origin_hash:016x}")`
/// routed through `core::fmt::Formatter`, which copies into a
/// stack buffer and walks the format-string machinery before
/// the `String` materializes. Post-fix is a direct 16-shift /
/// nibble-lookup unroll into a fresh 16-byte buffer that's
/// transmuted into `String` via `from_utf8(...).unwrap()` (the
/// lookup table only emits ASCII hex digits, so the UTF-8
/// validation is infallible by construction).
///
/// `collect_coverage` calls this exactly once per planning
/// call (line 1104) and threads the result through every per-
/// node tag scan; the speedup is sub-microsecond per planning
/// call, but the change keeps the hex-formatting pattern
/// consistent with dataforts #171 and removes a
/// `core::fmt`-shaped allocation from the planner hot path.
fn chain_hex(origin_hash: u64) -> String {
    let mut buf = [0u8; 16];
    let mut h = origin_hash;
    // Unrolled MSB-first nibble walk: byte 0 holds the most-
    // significant nibble of `origin_hash`, byte 15 the least.
    // Matches the `{:016x}` ordering byte-for-byte (pinned by
    // `chain_hex_matches_format_macro_byte_for_byte`).
    for i in (0..16).rev() {
        buf[i] = HEX_NIBBLES[(h & 0xF) as usize];
        h >>= 4;
    }
    // SAFETY: `HEX_NIBBLES` contains only ASCII hex digits, so
    // every byte written into `buf` is a valid UTF-8 byte and
    // `buf` is a valid UTF-8 string by construction. Skipping
    // the `from_utf8` validation walk is the whole point of the
    // lookup-table fix — the validation would re-scan 16 bytes
    // we already know are ASCII.
    unsafe { String::from_utf8_unchecked(buf.to_vec()) }
}

/// Parse a `causal:<hex>*` reserved tag, matching on the
/// supplied `origin_hex` stem. Returns `None` if the tag
/// isn't a `causal:` tag, the body's stem doesn't match,
/// or the variant suffix doesn't parse cleanly.
///
/// Recognized shapes (per `CAPABILITY_SYSTEM_PLAN.md` § 2):
///
/// - `causal:<hex>` → [`CausalClaim::Presence`]
/// - `causal:<hex>:<tip_seq>` → [`CausalClaim::Tip`]
/// - `causal:<hex>[<start>..<end>]` → [`CausalClaim::Range`]
///
/// Phase 3b: production walks have switched to
/// [`parse_causal_claim_str`] against fold-rendered string tags.
/// The Tag-based variant survives only for the legacy Tag-shape
/// parsing tests in this module; sub-step 3b-5 deletes it
/// alongside the legacy CapabilityIndex module.
#[allow(dead_code)]
fn parse_causal_claim(tag: &Tag, origin_hex: &str) -> Option<CausalClaim> {
    let Tag::Reserved { prefix, body } = tag else {
        return None;
    };
    if prefix != "causal:" {
        return None;
    }
    if !body.starts_with(origin_hex) {
        return None;
    }
    let rest = &body[origin_hex.len()..];
    if rest.is_empty() {
        return Some(CausalClaim::Presence);
    }
    if let Some(tip_str) = rest.strip_prefix(':') {
        // tip-form: parse the rest as decimal u64
        let tip: u64 = tip_str.parse().ok()?;
        return Some(CausalClaim::Tip {
            tip_seq: SeqNum(tip),
        });
    }
    if let Some(range_body) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        // range-form: `<start>..<end>` half-open
        let (start_str, end_str) = range_body.split_once("..")?;
        let start: u64 = start_str.parse().ok()?;
        let end: u64 = end_str.parse().ok()?;
        if start >= end {
            // Substrate emits half-open with `start < end`; a
            // degenerate range is malformed.
            return None;
        }
        return Some(CausalClaim::Range {
            start: SeqNum(start),
            end: SeqNum(end),
        });
    }
    // Unknown suffix shape — reject rather than partial-match.
    None
}

/// Extract the `u64` origin hash from a `causal:<hex>*`
/// reserved tag. Returns `None` if the tag isn't a `causal:`
/// reserved tag, the body's stem isn't 16 hex chars, or any
/// nibble fails to parse.
///
/// Used by `ChainRef::Discovered` resolution to map every
/// matched node's caps to its set of advertised origin hashes.
fn parse_causal_origin(tag: &Tag) -> Option<u64> {
    let Tag::Reserved { prefix, body } = tag else {
        return None;
    };
    if prefix != "causal:" {
        return None;
    }
    parse_causal_body(body)
}

/// String-form of [`parse_causal_claim`] — accepts the
/// canonical rendering used by the fold's
/// [`CapabilityMembership::tags`](crate::adapter::net::behavior::fold::CapabilityMembership).
/// Returns `None` for tags that don't start with `"causal:"`
/// or that target a different origin.
fn parse_causal_claim_str(tag: &str, origin_hex: &str) -> Option<CausalClaim> {
    let body = tag.strip_prefix("causal:")?;
    if !body.starts_with(origin_hex) {
        return None;
    }
    let rest = &body[origin_hex.len()..];
    if rest.is_empty() {
        return Some(CausalClaim::Presence);
    }
    if let Some(tip_str) = rest.strip_prefix(':') {
        let tip: u64 = tip_str.parse().ok()?;
        return Some(CausalClaim::Tip {
            tip_seq: SeqNum(tip),
        });
    }
    if let Some(range_body) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let (start_str, end_str) = range_body.split_once("..")?;
        let start: u64 = start_str.parse().ok()?;
        let end: u64 = end_str.parse().ok()?;
        if start >= end {
            return None;
        }
        return Some(CausalClaim::Range {
            start: SeqNum(start),
            end: SeqNum(end),
        });
    }
    None
}

/// String-form of [`parse_causal_origin`].
fn parse_causal_origin_str(tag: &str) -> Option<u64> {
    parse_causal_body(tag.strip_prefix("causal:")?)
}

/// String-form of `Tag::Reserved { prefix: "fork-of:", body }`
/// parsing — accepts the canonical fold rendering.
fn parse_fork_str(tag: &str) -> Option<u64> {
    parse_fork_body(tag.strip_prefix("fork-of:")?)
}

/// Parse a `causal:` body (everything after the `causal:`
/// prefix) into a `u64` origin hash. Strips the optional
/// `:<tip>` or `[start..end]` suffix before validating the
/// 16-hex-char stem.
fn parse_causal_body(body: &str) -> Option<u64> {
    let stem = body.split_once([':', '[']).map(|(s, _)| s).unwrap_or(body);
    if stem.len() != 16 {
        return None;
    }
    u64::from_str_radix(stem, 16).ok()
}

/// Parse a `fork-of:<16-hex>` reserved tag's body into a `u64`
/// parent origin hash. Returns `None` for any non-conforming
/// body (wrong length, non-hex). Mirrors [`parse_causal_origin`]'s
/// strictness so the lineage walk has the same shape contract as
/// causal-tag parsing.
fn parse_fork_body(body: &str) -> Option<u64> {
    if body.len() != 16 {
        return None;
    }
    u64::from_str_radix(body, 16).ok()
}

/// Derive the [`JoinKeyMode`] from a [`JoinKey`].
///
/// Both sides must agree on the same field name. Row-intrinsic
/// names (`"origin"`, `"seq"`, `"origin,seq"`) map to the
/// matching `JoinKeyMode` enum variant; anything else is
/// treated as a JSON payload path and maps to
/// `JoinKeyMode::Field(path)` (Phase D-2 row-schema decoding).
fn key_mode_for_join(on: &JoinKey) -> Result<JoinKeyMode, MeshError> {
    let left = field_name(&on.left_field).ok_or_else(|| MeshError::PlannerError {
        detail: format!(
            "join left key must be a field reference, got {:?}",
            on.left_field
        ),
    })?;
    let right = field_name(&on.right_field).ok_or_else(|| MeshError::PlannerError {
        detail: format!(
            "join right key must be a field reference, got {:?}",
            on.right_field
        ),
    })?;
    if left != right {
        return Err(MeshError::PlannerError {
            detail: format!(
                "join key sides must reference the same field name (left='{left}', right='{right}')"
            ),
        });
    }
    Ok(match left {
        "origin" => JoinKeyMode::Origin,
        "seq" => JoinKeyMode::Seq,
        "origin,seq" => JoinKeyMode::OriginSeq,
        other => JoinKeyMode::Field(other.to_string()),
    })
}

/// Borrow the inner `&str` of an [`Expr::Field`]. Returns
/// `None` for any other variant.
fn field_name(e: &Expr) -> Option<&str> {
    match e {
        Expr::Field(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Pull the field path out of an `Expr::Field` (which numeric
/// aggregates require for their target field). Surfaces a
/// `PlannerError` for any other expression shape.
fn field_path_required(e: &Expr, op_name: &str) -> Result<String, MeshError> {
    match e {
        Expr::Field(s) => Ok(s.clone()),
        other => Err(MeshError::PlannerError {
            detail: format!(
                "{op_name} requires Expr::Field(<path>) for its field argument, got {other:?}"
            ),
        }),
    }
}

/// Resolve a `group_by: Vec<Expr>` clause to a Phase E-1
/// row-intrinsic [`JoinKeyMode`].
///
/// Returns `Ok(None)` for an empty `group_by` (single-bucket
/// aggregate). Surfaces `PlannerError` for any payload-keyed
/// or composite group_by we don't support yet.
fn group_by_mode(group_by: &[Expr]) -> Result<Option<JoinKeyMode>, MeshError> {
    if group_by.is_empty() {
        return Ok(None);
    }
    if group_by.len() == 1 {
        let name = field_name(&group_by[0]).ok_or_else(|| MeshError::PlannerError {
            detail: format!(
                "group_by[0] must be a field reference, got {:?}",
                group_by[0]
            ),
        })?;
        return match name {
            "origin" => Ok(Some(JoinKeyMode::Origin)),
            "seq" => Ok(Some(JoinKeyMode::Seq)),
            other => Err(MeshError::PlannerError {
                detail: format!(
                    "group_by field '{other}' is not a row-intrinsic key; only 'origin' / 'seq' supported in Phase E-1"
                ),
            }),
        };
    }
    if group_by.len() == 2 {
        let l = field_name(&group_by[0]);
        let r = field_name(&group_by[1]);
        if matches!(
            (l, r),
            (Some("origin"), Some("seq")) | (Some("seq"), Some("origin"))
        ) {
            return Ok(Some(JoinKeyMode::OriginSeq));
        }
    }
    Err(MeshError::PlannerError {
        detail: format!(
            "group_by shape {group_by:?} not supported in Phase E-1; only [origin], [seq], or [origin, seq] are row-intrinsic"
        ),
    })
}

// Silence unused-import warning under feature-conditional
// configurations of the planner. `TaxonomyAxis` is held for
// future reference by Phase B's discovery-time match_axis
// path; `CapabilityQuery` is the trait the index implements.
#[allow(dead_code)]
const _PLANNER_USES_TAXONOMY_AXIS: TaxonomyAxis = TaxonomyAxis::Dataforts;
#[allow(dead_code)]
fn _planner_uses_capability_query<Q: CapabilityQuery>(_q: &Q) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        CapabilityAnnouncement, CapabilityIndex, CapabilitySet,
    };
    use crate::adapter::net::behavior::fold::capability_bridge;
    use crate::adapter::net::identity::EntityId;

    /// Pin meshdb perf #211: the nibble-lookup `chain_hex` must
    /// produce the SAME 16-char lowercase-hex string as the
    /// legacy `format!("{origin_hash:016x}")` for every input.
    /// `causal:<hex>` tag bodies are matched on by stem (see
    /// `parse_causal_claim`), so a single-byte divergence
    /// would silently break causal-claim discovery — every
    /// announced `causal:<hex>` tag would fail to match the
    /// planner's derived stem and the federated query layer
    /// would behave as if no holders existed for that chain.
    /// Exhaustive comparison across boundary, single-bit, and
    /// arbitrary u64 inputs.
    #[test]
    fn chain_hex_matches_format_macro_byte_for_byte() {
        let cases: &[u64] = &[
            0,
            1,
            0xF,
            0x10,
            0xFF,
            0x100,
            0xDEAD_BEEF,
            0x8000_0000_0000_0000,
            0x7FFF_FFFF_FFFF_FFFF,
            u64::MAX,
            u64::MAX - 1,
            // A bunch of arbitrary mid-range values to exercise
            // each of the 16 nibble positions.
            0x0123_4567_89AB_CDEF,
            0xFEDC_BA98_7654_3210,
            0xCAFE_BABE_DEAD_BEEF,
            0x1234_5678_9ABC_DEF0,
        ];
        for &h in cases {
            let reference = format!("{h:016x}");
            let actual = chain_hex(h);
            assert_eq!(
                actual, reference,
                "chain_hex({h:#x}) diverged from format!(\"{{:016x}}\")",
            );
            assert_eq!(actual.len(), 16, "chain_hex must always emit 16 chars");
        }
    }

    /// Build a single `causal:<body>` reserved tag from the
    /// supplied body string. The `add_tag` builder silently
    /// drops reserved-prefix tags (it routes through
    /// `Tag::parse_user`), so the tests build the tag directly
    /// via `Tag::Reserved` to mimic what
    /// `MeshNode::announce_chain` / `announce_chain_range`
    /// emits at runtime.
    fn causal_tag(body: impl Into<String>) -> Tag {
        Tag::Reserved {
            prefix: "causal:".to_string(),
            body: body.into(),
        }
    }

    /// Build a fresh capability set carrying a single
    /// `causal:<hex>` presence-form tag for `origin_hash`.
    fn caps_with_causal_presence(origin_hash: u64) -> CapabilitySet {
        let mut caps = CapabilitySet::new();
        caps.tags.insert(causal_tag(chain_hex(origin_hash)));
        caps
    }

    /// Build a fresh capability set carrying a single
    /// `causal:<hex>:<tip>` tip-form tag for `origin_hash`.
    fn caps_with_causal_tip(origin_hash: u64, tip: u64) -> CapabilitySet {
        let mut caps = CapabilitySet::new();
        caps.tags
            .insert(causal_tag(format!("{}:{}", chain_hex(origin_hash), tip)));
        caps
    }

    /// Build a fresh capability set carrying a single
    /// `causal:<hex>[start..end]` range-form tag.
    fn caps_with_causal_range(origin_hash: u64, start: u64, end: u64) -> CapabilitySet {
        let mut caps = CapabilitySet::new();
        caps.tags.insert(causal_tag(format!(
            "{}[{}..{}]",
            chain_hex(origin_hash),
            start,
            end
        )));
        caps
    }

    /// Build a `(Fold<CapabilityFold>, CapabilityIndex)` pair
    /// populated identically — the same announcement applied to
    /// both. Mirrors the Phase 3b dual-population invariant the
    /// production mesh maintains.
    fn index_with(holders: Vec<(u64, CapabilitySet)>) -> (Fold<CapabilityFold>, CapabilityIndex) {
        let index = CapabilityIndex::new();
        let fold = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
        for (node_id, caps) in holders {
            capability_bridge::dual_apply(
                &fold,
                &index,
                CapabilityAnnouncement::new(
                    node_id,
                    EntityId::from_bytes([node_id as u8; 32]),
                    1,
                    caps,
                ),
            );
        }
        (fold, index)
    }

    fn make_index_with_holder(
        node_id: u64,
        origin_hash: u64,
    ) -> (Fold<CapabilityFold>, CapabilityIndex) {
        index_with(vec![(node_id, caps_with_causal_presence(origin_hash))])
    }

    fn empty_index() -> (Fold<CapabilityFold>, CapabilityIndex) {
        (
            Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO),
            CapabilityIndex::new(),
        )
    }

    fn rtt_none(_nid: u64) -> Option<Duration> {
        None
    }

    // ========================================================================
    // CausalClaim parsing + coverage semantics
    // ========================================================================

    #[test]
    fn parse_causal_presence_form() {
        let origin = 0xDEAD_BEEF_CAFE_BABE_u64;
        let hex = chain_hex(origin);
        let claim = parse_causal_claim(&causal_tag(hex.clone()), &hex);
        assert_eq!(claim, Some(CausalClaim::Presence));
    }

    #[test]
    fn parse_causal_tip_form() {
        let origin = 0x1234_5678_9ABC_DEF0_u64;
        let hex = chain_hex(origin);
        let claim = parse_causal_claim(&causal_tag(format!("{hex}:1000")), &hex);
        assert_eq!(
            claim,
            Some(CausalClaim::Tip {
                tip_seq: SeqNum(1000)
            })
        );
    }

    #[test]
    fn parse_causal_range_form() {
        let origin = 0xAAAA_BBBB_CCCC_DDDD_u64;
        let hex = chain_hex(origin);
        let claim = parse_causal_claim(&causal_tag(format!("{hex}[100..500]")), &hex);
        assert_eq!(
            claim,
            Some(CausalClaim::Range {
                start: SeqNum(100),
                end: SeqNum(500),
            })
        );
    }

    #[test]
    fn parse_causal_rejects_inverted_range() {
        // Degenerate `[start..end]` with `start >= end` is
        // malformed per the substrate emitter's contract.
        let hex = chain_hex(1);
        let claim = parse_causal_claim(&causal_tag(format!("{hex}[500..100]")), &hex);
        assert_eq!(claim, None);
    }

    #[test]
    fn parse_causal_rejects_unknown_suffix() {
        let hex = chain_hex(1);
        let claim = parse_causal_claim(&causal_tag(format!("{hex}?weird")), &hex);
        assert_eq!(claim, None);
    }

    #[test]
    fn parse_causal_rejects_wrong_hash() {
        // `causal:<otherhex>:42` shouldn't match a query for
        // a different chain even if it parses.
        let other_hex = chain_hex(0xFFFF);
        let claim = parse_causal_claim(&causal_tag(format!("{other_hex}:42")), &chain_hex(0xAAAA));
        assert_eq!(claim, None);
    }

    #[test]
    fn causal_claim_covers_seq_semantics() {
        assert!(CausalClaim::Presence.covers_seq(SeqNum(0)));
        assert!(CausalClaim::Presence.covers_seq(SeqNum(u64::MAX)));

        let tip = CausalClaim::Tip {
            tip_seq: SeqNum(100),
        };
        assert!(tip.covers_seq(SeqNum(0)));
        assert!(tip.covers_seq(SeqNum(100)));
        assert!(!tip.covers_seq(SeqNum(101)));

        let range = CausalClaim::Range {
            start: SeqNum(50),
            end: SeqNum(150),
        };
        assert!(!range.covers_seq(SeqNum(49)));
        assert!(range.covers_seq(SeqNum(50)));
        assert!(range.covers_seq(SeqNum(149)));
        assert!(!range.covers_seq(SeqNum(150))); // half-open
    }

    #[test]
    fn causal_claim_covers_range_semantics() {
        assert!(CausalClaim::Presence.covers_range(SeqNum(0), SeqNum(1_000)));

        let tip = CausalClaim::Tip {
            tip_seq: SeqNum(100),
        };
        // Tip covers [0, 101); requested end must be <= 101.
        assert!(tip.covers_range(SeqNum(0), SeqNum(101)));
        assert!(tip.covers_range(SeqNum(50), SeqNum(101)));
        assert!(!tip.covers_range(SeqNum(0), SeqNum(102)));

        let range = CausalClaim::Range {
            start: SeqNum(100),
            end: SeqNum(200),
        };
        assert!(range.covers_range(SeqNum(100), SeqNum(200)));
        assert!(range.covers_range(SeqNum(150), SeqNum(175)));
        assert!(!range.covers_range(SeqNum(50), SeqNum(150))); // starts below
        assert!(!range.covers_range(SeqNum(150), SeqNum(250))); // ends above
    }

    #[test]
    fn causal_claim_advertised_renders_half_open_range() {
        assert_eq!(CausalClaim::Presence.advertised(), None);
        assert_eq!(
            (CausalClaim::Tip {
                tip_seq: SeqNum(99)
            })
            .advertised(),
            Some(SeqNum(0)..SeqNum(100))
        );
        assert_eq!(
            (CausalClaim::Range {
                start: SeqNum(10),
                end: SeqNum(50),
            })
            .advertised(),
            Some(SeqNum(10)..SeqNum(50))
        );
    }

    #[test]
    fn causal_claim_latest_tip_ordering() {
        assert_eq!(CausalClaim::Presence.latest_tip(), None);
        assert_eq!(
            (CausalClaim::Tip {
                tip_seq: SeqNum(42)
            })
            .latest_tip(),
            Some(SeqNum(42))
        );
        // Range advertises `[start, end)` — latest is end-1.
        assert_eq!(
            (CausalClaim::Range {
                start: SeqNum(10),
                end: SeqNum(50),
            })
            .latest_tip(),
            Some(SeqNum(49))
        );
    }

    #[test]
    fn parse_causal_origin_extracts_u64_from_each_form() {
        let origin = 0xCAFE_BABE_DEAD_BEEF_u64;
        let hex = chain_hex(origin);

        // presence form
        assert_eq!(parse_causal_origin(&causal_tag(hex.clone())), Some(origin));
        // tip form
        assert_eq!(
            parse_causal_origin(&causal_tag(format!("{hex}:42"))),
            Some(origin)
        );
        // range form
        assert_eq!(
            parse_causal_origin(&causal_tag(format!("{hex}[0..100]"))),
            Some(origin)
        );
        // non-causal tag
        assert_eq!(
            parse_causal_origin(&Tag::Reserved {
                prefix: "heat:".to_string(),
                body: hex.clone(),
            }),
            None
        );
        // wrong-length stem
        assert_eq!(parse_causal_origin(&causal_tag("abc".to_string())), None);
    }

    // ========================================================================
    // Atomic-operator planning (At / Between / Latest)
    // ========================================================================

    #[test]
    fn plan_latest_returns_atomic_with_holder() {
        let origin = 0xABAB_ABAB_ABAB_ABAB_u64;
        let (fold, _index) = make_index_with_holder(42, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            }))
            .expect("plan ok");
        match plan.root.operator {
            OperatorPlan::LatestRead { origin: o } => assert_eq!(o, origin),
            other => panic!("expected LatestRead; got {other:?}"),
        }
        assert_eq!(plan.root.target_nodes, vec![42]);
    }

    #[test]
    fn plan_latest_with_no_holders_returns_empty_targets() {
        // When no holder advertises the chain at all, the
        // planner emits an empty target list rather than
        // failing — the executor surfaces
        // `HistoricalRangeUnavailable` against that empty
        // set. (Phase A semantics; preserved in Phase B.)
        let (fold, _index) = empty_index();
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(0),
            }))
            .expect("plan ok");
        assert!(plan.root.target_nodes.is_empty());
    }

    #[test]
    fn plan_between_rejects_inverted_range() {
        let (fold, _index) = empty_index();
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::Between {
                origin: ChainRef::OriginHash(0),
                start: SeqNum(100),
                end: SeqNum(50),
            }))
            .expect_err("inverted range must fail");
        match err {
            MeshError::PlannerError { detail } => assert!(detail.contains("start < end")),
            other => panic!("expected PlannerError; got {other:?}"),
        }
    }

    #[test]
    fn plan_between_accepts_valid_range_with_covering_holder() {
        // Holder advertises tip 1000 → covers [0, 1001). The
        // requested [0, 1000) fits.
        let origin = 0x4242_4242_4242_4242_u64;
        let (fold, _index) = index_with(vec![(7, caps_with_causal_tip(origin, 1000))]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Between {
                origin: ChainRef::OriginHash(origin),
                start: SeqNum(0),
                end: SeqNum(1000),
            }))
            .expect("plan ok");
        match plan.root.operator {
            OperatorPlan::BetweenRead { start, end, .. } => {
                assert_eq!(start, SeqNum(0));
                assert_eq!(end, SeqNum(1000));
            }
            other => panic!("expected BetweenRead; got {other:?}"),
        }
        assert_eq!(plan.root.target_nodes, vec![7]);
    }

    #[test]
    fn plan_at_routes_to_holder() {
        let origin = 0xCCCC_CCCC_CCCC_CCCC_u64;
        let (fold, _index) = make_index_with_holder(99, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::At {
                origin: ChainRef::OriginHash(origin),
                seq: SeqNum(7),
            }))
            .expect("plan ok");
        match plan.root.operator {
            OperatorPlan::AtRead { origin: o, seq } => {
                assert_eq!(o, origin);
                assert_eq!(seq, SeqNum(7));
            }
            other => panic!("expected AtRead; got {other:?}"),
        }
        assert_eq!(plan.root.target_nodes, vec![99]);
    }

    #[test]
    fn plan_holders_lex_sorted_when_no_rtt() {
        // No RTT data → fall back to lex-NodeId order for
        // determinism. Three holders inserted in non-monotonic
        // order; planner sort restores lex.
        let origin = 0xEEEE_EEEE_EEEE_EEEE_u64;
        let caps = caps_with_causal_presence(origin);
        let (fold, _index) = index_with(vec![(200, caps.clone()), (50, caps.clone()), (100, caps)]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            }))
            .unwrap();
        assert_eq!(plan.root.target_nodes, vec![50, 100, 200]);
    }

    // ========================================================================
    // Phase B — replica-aware routing
    // ========================================================================

    #[test]
    fn at_picks_holder_whose_tip_covers_seq() {
        // Two holders: one with tip 50, one with tip 200.
        // Query `At(100)` — only the tip-200 holder covers.
        let origin = 0x1111_2222_3333_4444_u64;
        let (fold, _index) = index_with(vec![
            (50, caps_with_causal_tip(origin, 50)),
            (200, caps_with_causal_tip(origin, 200)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::At {
                origin: ChainRef::OriginHash(origin),
                seq: SeqNum(100),
            }))
            .unwrap();
        assert_eq!(plan.root.target_nodes, vec![200]);
    }

    #[test]
    fn between_picks_only_holders_with_full_coverage() {
        // Three holders. Query `Between(100, 500)`.
        // - holder A: range [0..400] — doesn't cover (end<500)
        // - holder B: range [50..600] — covers
        // - holder C: tip 700 — covers (full prefix up to 700)
        let origin = 0xFEED_FACE_FEED_FACE_u64;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_range(origin, 0, 400)),
            (2, caps_with_causal_range(origin, 50, 600)),
            (3, caps_with_causal_tip(origin, 700)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Between {
                origin: ChainRef::OriginHash(origin),
                start: SeqNum(100),
                end: SeqNum(500),
            }))
            .unwrap();
        // Holders B + C qualify; lex-sort puts them as [2, 3].
        assert_eq!(plan.root.target_nodes, vec![2, 3]);
    }

    #[test]
    fn between_surfaces_historical_range_unavailable_with_hints() {
        // No holder covers the full requested range; planner
        // surfaces `HistoricalRangeUnavailable` carrying the
        // available-range hints for caller renegotiation.
        let origin = 0xDEAD_DEAD_DEAD_DEAD_u64;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_range(origin, 0, 100)),
            (2, caps_with_causal_tip(origin, 50)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::Between {
                origin: ChainRef::OriginHash(origin),
                start: SeqNum(0),
                end: SeqNum(500),
            }))
            .expect_err("no holder covers [0, 500)");
        match err {
            MeshError::HistoricalRangeUnavailable {
                origin: o,
                requested,
                available,
            } => {
                assert_eq!(o, origin);
                assert_eq!(requested, SeqNum(0)..SeqNum(500));
                // Both holders' advertised ranges surface as
                // hints. Order: per-coverage-list (proximity
                // then lex); both unmeasured here so lex.
                assert_eq!(
                    available,
                    vec![SeqNum(0)..SeqNum(100), SeqNum(0)..SeqNum(51)]
                );
            }
            other => panic!("expected HistoricalRangeUnavailable; got {other:?}"),
        }
    }

    #[test]
    fn at_surfaces_historical_range_unavailable_when_no_coverage() {
        // Holder advertises tip 50; query asks for seq 100.
        let origin = 0xBABE_BABE_BABE_BABE_u64;
        let (fold, _index) = index_with(vec![(1, caps_with_causal_tip(origin, 50))]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::At {
                origin: ChainRef::OriginHash(origin),
                seq: SeqNum(100),
            }))
            .expect_err("seq beyond tip");
        match err {
            MeshError::HistoricalRangeUnavailable {
                requested,
                available,
                ..
            } => {
                // Requested rendered as a single-seq range.
                assert_eq!(requested, SeqNum(100)..SeqNum(101));
                assert_eq!(available, vec![SeqNum(0)..SeqNum(51)]);
            }
            other => panic!("expected HistoricalRangeUnavailable; got {other:?}"),
        }
    }

    #[test]
    fn presence_form_holder_is_permissive_fallback() {
        // A holder advertising bare `causal:<hex>` (no range
        // claim) is admitted permissively — it makes no
        // claim about coverage, so the executor will
        // attempt the read and surface
        // HistoricalRangeUnavailable if the read actually
        // fails. Phase B planner trusts the presence claim.
        let origin = 0xFADE_FADE_FADE_FADE_u64;
        let (fold, _index) = index_with(vec![(1, caps_with_causal_presence(origin))]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::At {
                origin: ChainRef::OriginHash(origin),
                seq: SeqNum(999_999),
            }))
            .unwrap();
        assert_eq!(plan.root.target_nodes, vec![1]);
    }

    #[test]
    fn latest_prefers_holder_with_highest_tip() {
        // Three holders with tips 50, 500, 200. Latest picks
        // the holder with the highest tip first.
        let origin = 0xCAFE_CAFE_CAFE_CAFE_u64;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_tip(origin, 50)),
            (2, caps_with_causal_tip(origin, 500)),
            (3, caps_with_causal_tip(origin, 200)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            }))
            .unwrap();
        // Descending tip: 500 (node 2) > 200 (node 3) > 50 (node 1).
        assert_eq!(plan.root.target_nodes, vec![2, 3, 1]);
    }

    #[test]
    fn proximity_ordering_breaks_lex_default() {
        // Three holders, all with bare presence claims. RTTs
        // are 30ms, 10ms, 20ms for node_ids 100, 50, 200
        // respectively. Lex order would be [50, 100, 200];
        // proximity puts them in [50, 200, 100] order.
        let origin = 0x3030_3030_3030_3030_u64;
        let caps = caps_with_causal_presence(origin);
        let (fold, _index) = index_with(vec![(100, caps.clone()), (50, caps.clone()), (200, caps)]);
        let rtt = |nid: u64| {
            Some(match nid {
                50 => Duration::from_millis(10),
                200 => Duration::from_millis(20),
                100 => Duration::from_millis(30),
                _ => return None::<Duration>,
            })
        };
        let planner = MeshQueryPlanner::new(&fold,rtt);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            }))
            .unwrap();
        assert_eq!(plan.root.target_nodes, vec![50, 200, 100]);
    }

    #[test]
    fn unmeasured_rtt_falls_last_lex_among_themselves() {
        // RTT data exists for some holders, not others.
        // Measured holders sort by RTT; unmeasured holders
        // sort lex and land after every measured one.
        let origin = 0x7070_7070_7070_7070_u64;
        let caps = caps_with_causal_presence(origin);
        let (fold, _index) = index_with(vec![
            (1, caps.clone()),
            (2, caps.clone()),
            (3, caps.clone()),
            (4, caps),
        ]);
        let rtt = |nid: u64| match nid {
            2 => Some(Duration::from_millis(5)),
            3 => Some(Duration::from_millis(15)),
            _ => None,
        };
        let planner = MeshQueryPlanner::new(&fold,rtt);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            }))
            .unwrap();
        // Measured: [2 (5ms), 3 (15ms)]; unmeasured: [1, 4].
        assert_eq!(plan.root.target_nodes, vec![2, 3, 1, 4]);
    }

    #[test]
    fn coverage_picks_most_specific_claim_when_holder_advertises_multiple() {
        // One holder advertises BOTH presence AND tip 100.
        // The planner picks the most specific (tip) so the
        // coverage check uses the tighter claim.
        let origin = 0x6060_6060_6060_6060_u64;
        let hex = chain_hex(origin);
        let mut caps = CapabilitySet::new();
        caps.tags.insert(causal_tag(hex.clone())); // presence
        caps.tags.insert(causal_tag(format!("{hex}:100"))); // tip 100
        let (fold, _index) = index_with(vec![(7, caps)]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        // Query At(150) — only presence would qualify
        // (permissive), but tip's `seq <= 100` rejects.
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::At {
                origin: ChainRef::OriginHash(origin),
                seq: SeqNum(150),
            }))
            .expect_err("most-specific claim (tip) should not cover seq 150");
        assert!(matches!(err, MeshError::HistoricalRangeUnavailable { .. }));
    }

    // ========================================================================
    // ChainRef::Discovered resolution
    // ========================================================================

    #[test]
    fn plan_chainref_discovered_resolves_via_filter() {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let origin = 0xCAFE_BEEF_CAFE_BEEF_u64;
        let hex = chain_hex(origin);
        let mut caps = CapabilitySet::new().add_tag("dataforts.blob.storage");
        caps.tags.insert(causal_tag(hex));
        let (fold, _index) = index_with(vec![(42, caps)]);
        let pred = Predicate::Exists {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            },
        };
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::Discovered(pred.to_wire()),
            }))
            .expect("Discovered resolution should succeed");
        match plan.root.operator {
            OperatorPlan::LatestRead { origin: o } => assert_eq!(o, origin),
            other => panic!("expected LatestRead; got {other:?}"),
        }
        assert_eq!(plan.root.target_nodes, vec![42]);
    }

    #[test]
    fn plan_chainref_discovered_no_match_returns_no_capable_holder() {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let pred = Predicate::Exists {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            },
        };
        let (fold, _index) = empty_index();
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::Discovered(pred.to_wire()),
            }))
            .expect_err("Discovered against empty index must surface NoCapableHolder");
        match err {
            MeshError::NoCapableHolder { requirement, .. } => {
                assert!(requirement.contains("Exists"));
            }
            other => panic!("expected NoCapableHolder; got {other:?}"),
        }
    }

    #[test]
    fn plan_chainref_discovered_multiple_origins_surfaces_ambiguous_error() {
        // Regression: previously resolve_origin silently took
        // the lex-smallest match when the predicate hit more
        // than one origin. Until Phase B's fan-out lands, that
        // returns wrong rows; we now surface AmbiguousDiscovery
        // and let the caller tighten the predicate.
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let origin_a = 0x0000_AAAA_AAAA_AAAA_u64;
        let origin_b = 0x0000_BBBB_BBBB_BBBB_u64;
        let mut caps_a = CapabilitySet::new().add_tag("dataforts.blob.storage");
        caps_a.tags.insert(causal_tag(chain_hex(origin_a)));
        let mut caps_b = CapabilitySet::new().add_tag("dataforts.blob.storage");
        caps_b.tags.insert(causal_tag(chain_hex(origin_b)));
        let (fold, _index) = index_with(vec![(1, caps_a), (2, caps_b)]);
        let pred = Predicate::Exists {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            },
        };
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::Discovered(pred.to_wire()),
            }))
            .expect_err("two-origin discovery should surface AmbiguousDiscovery");
        match err {
            MeshError::AmbiguousDiscovery { matches, .. } => {
                assert_eq!(matches.len(), 2);
                assert!(matches.contains(&origin_a));
                assert!(matches.contains(&origin_b));
            }
            other => panic!("expected AmbiguousDiscovery; got {other:?}"),
        }
    }

    #[test]
    fn plan_chainref_discovered_match_with_no_causal_tag_surfaces_no_capable_holder() {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let caps = CapabilitySet::new().add_tag("dataforts.blob.storage");
        let (fold, _index) = index_with(vec![(7, caps)]);
        let pred = Predicate::Exists {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            },
        };
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::Discovered(pred.to_wire()),
            }))
            .expect_err("missing causal: tag should surface NoCapableHolder");
        assert!(matches!(err, MeshError::NoCapableHolder { .. }));
    }

    // ========================================================================
    // Composite operators + determinism + round-trip
    // ========================================================================

    #[test]
    fn plan_composite_operator_surfaces_not_yet_implemented() {
        // Project remains deferred (Phase A.2 placeholder until a
        // consumer drives column-extraction semantics). Use it as
        // the canonical "wrapped sub-plan flows through
        // NotYetImplemented" test now that Aggregate Count ships.
        let origin = 0x9999_9999_9999_9999_u64;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let q = MeshQuery::V1(QueryV1::Project {
            inner: Box::new(MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            })),
            columns: vec![Expr::Field("origin".to_string())],
        });
        let plan = planner.plan(&q).unwrap();
        match plan.root.operator {
            OperatorPlan::NotYetImplemented { detail, input } => {
                assert!(detail.contains("Project"));
                assert!(input.is_some(), "Project's inner sub-plan must be carried");
            }
            other => panic!("expected NotYetImplemented; got {other:?}"),
        }
    }

    #[test]
    fn plan_is_deterministic() {
        // Same query + same index → byte-identical encoded
        // plan. Load-bearing for the locked-decision-#4
        // cache key.
        let origin = 0x5555_5555_5555_5555_u64;
        let (fold, _index) = make_index_with_holder(11, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let q = MeshQuery::V1(QueryV1::Latest {
            origin: ChainRef::OriginHash(origin),
        });
        let p1 = planner.plan(&q).unwrap();
        let p2 = planner.plan(&q).unwrap();
        let e1 = postcard::to_allocvec(&p1).unwrap();
        let e2 = postcard::to_allocvec(&p2).unwrap();
        assert_eq!(e1, e2, "plan must be deterministic byte-by-byte");
    }

    #[test]
    fn lineage_back_with_multiple_fork_of_tags_is_deterministic() {
        // Regression: caps.tags is a HashSet. A node carrying
        // several `fork-of:` tags previously surfaced "first
        // in iteration order" which varies run-to-run. The
        // planner now sorts the candidates numerically. Build
        // a host that lists three forks and assert the BFS
        // plan + cache key are byte-stable across 32 fresh
        // planners.
        let child = 0x0000_0000_0000_00CC_u64;
        let parent_a = 0x0000_0000_0000_0001_u64;
        let parent_b = 0x0000_0000_0000_0002_u64;
        let parent_c = 0x0000_0000_0000_0003_u64;
        let mut caps = CapabilitySet::new();
        caps.tags.insert(causal_tag(chain_hex(child)));
        caps.tags.insert(fork_tag(parent_a));
        caps.tags.insert(fork_tag(parent_b));
        caps.tags.insert(fork_tag(parent_c));
        // Root nodes for each candidate parent so the BFS
        // resolves a parent regardless of which one wins.
        let (fold, _index) = index_with(vec![
            (1, caps),
            (2, caps_chain_only(parent_a)),
            (3, caps_chain_only(parent_b)),
            (4, caps_chain_only(parent_c)),
        ]);

        let mut last_encoding: Option<Vec<u8>> = None;
        for _ in 0..32 {
            let planner = MeshQueryPlanner::new(&fold,rtt_none);
            let plan = planner
                .plan(&MeshQuery::V1(QueryV1::LineageBack {
                    origin: ChainRef::OriginHash(child),
                    max_depth: 1,
                }))
                .unwrap();
            let bytes = postcard::to_allocvec(&plan).unwrap();
            if let Some(prev) = &last_encoding {
                assert_eq!(
                    prev, &bytes,
                    "BFS plan must not depend on HashSet iter order"
                );
            }
            last_encoding = Some(bytes);
        }
    }

    #[test]
    fn lineage_back_across_multiple_replica_hosts_is_deterministic() {
        // Regression: pass-1 M1 made intra-node tag selection
        // deterministic, but the outer parent_of loop still
        // short-circuited on the first DashMap-iterated node
        // that hosted the child via a `causal:` tag. Two nodes
        // replicating the same chain origin with DIFFERENT
        // fork-of declarations could produce different plans
        // across runs.
        let child = 0x0000_0000_0000_DDDD_u64;
        let parent_a = 0x0000_0000_0000_0001_u64;
        let parent_b = 0x0000_0000_0000_0002_u64;
        // Two replicas of `child`; each declares a different parent.
        let (fold, _index) = index_with(vec![
            (1, caps_chain_forked_from(child, parent_a)),
            (2, caps_chain_forked_from(child, parent_b)),
            (3, caps_chain_only(parent_a)),
            (4, caps_chain_only(parent_b)),
        ]);

        let mut last_encoding: Option<Vec<u8>> = None;
        for _ in 0..32 {
            let planner = MeshQueryPlanner::new(&fold,rtt_none);
            let plan = planner
                .plan(&MeshQuery::V1(QueryV1::LineageBack {
                    origin: ChainRef::OriginHash(child),
                    max_depth: 1,
                }))
                .unwrap();
            let bytes = postcard::to_allocvec(&plan).unwrap();
            if let Some(prev) = &last_encoding {
                assert_eq!(
                    prev, &bytes,
                    "parent_of must not depend on DashMap iter order across replicas",
                );
            }
            last_encoding = Some(bytes);
        }
    }

    #[test]
    fn execution_plan_round_trips_through_postcard() {
        let origin = 0x1111_1111_1111_1111_u64;
        let (fold, _index) = make_index_with_holder(3, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let p = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            }))
            .unwrap();
        let bytes = postcard::to_allocvec(&p).unwrap();
        let back: ExecutionPlan = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn cost_estimate_propagates_rtt() {
        let origin = 0x2222_2222_2222_2222_u64;
        let (fold, _index) = make_index_with_holder(5, origin);
        let rtt = |nid: u64| {
            if nid == 5 {
                Some(Duration::from_millis(15))
            } else {
                None
            }
        };
        let planner = MeshQueryPlanner::new(&fold,rtt);
        let p = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            }))
            .unwrap();
        assert_eq!(p.root.cost.latency_ms, 15);
        assert_eq!(p.root.cost.bandwidth_bytes, PHASE_A_ATOMIC_BANDWIDTH_BYTES);
    }

    // ========================================================================
    // Phase C — lineage walks
    // ========================================================================

    /// Build a `fork-of:<hex>` reserved tag from a parent
    /// origin hash. Mirrors `causal_tag` — `add_tag` would
    /// silently drop reserved-prefix tags (routes through
    /// `parse_user`), so build `Tag::Reserved` directly.
    fn fork_tag(parent_hash: u64) -> Tag {
        Tag::Reserved {
            prefix: "fork-of:".to_string(),
            body: chain_hex(parent_hash),
        }
    }

    /// Capability set advertising `chain` plus a `fork-of:`
    /// declaration pointing at `parent`. Models "this host
    /// holds chain X which is forked from chain P".
    fn caps_chain_forked_from(chain: u64, parent: u64) -> CapabilitySet {
        let mut caps = CapabilitySet::new();
        caps.tags.insert(causal_tag(chain_hex(chain)));
        caps.tags.insert(fork_tag(parent));
        caps
    }

    /// Capability set advertising `chain` plus a tip + a
    /// `fork-of:` declaration. Used to verify `tip_seq`
    /// propagation through lineage entries.
    fn caps_chain_tip_forked_from(chain: u64, tip: u64, parent: u64) -> CapabilitySet {
        let mut caps = CapabilitySet::new();
        caps.tags
            .insert(causal_tag(format!("{}:{}", chain_hex(chain), tip)));
        caps.tags.insert(fork_tag(parent));
        caps
    }

    /// Capability set advertising just `chain` (no fork-of:).
    /// Models the "root chain" — has no parent.
    fn caps_chain_only(chain: u64) -> CapabilitySet {
        let mut caps = CapabilitySet::new();
        caps.tags.insert(causal_tag(chain_hex(chain)));
        caps
    }

    #[test]
    fn parse_fork_body_round_trips_16_hex() {
        assert_eq!(parse_fork_body("00000000deadbeef"), Some(0xDEAD_BEEF));
        assert_eq!(
            parse_fork_body(&chain_hex(0x1234_5678_9ABC_DEF0)),
            Some(0x1234_5678_9ABC_DEF0)
        );
    }

    #[test]
    fn parse_fork_body_rejects_short_or_non_hex() {
        assert!(parse_fork_body("deadbeef").is_none()); // too short
        assert!(parse_fork_body("deadbeefcafebabe0").is_none()); // too long
        assert!(parse_fork_body("zzzzzzzzzzzzzzzz").is_none()); // non-hex
    }

    #[test]
    fn lineage_back_single_root_returns_only_start() {
        let root = 0x0000_0000_0000_0001_u64;
        let (fold, _index) = index_with(vec![(1, caps_chain_only(root))]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(root),
                max_depth: 5,
            }))
            .unwrap();
        match plan.root.operator {
            OperatorPlan::LineageEmit {
                origin,
                direction,
                entries,
            } => {
                assert_eq!(origin, root);
                assert_eq!(direction, LineageDirection::Back);
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].origin, root);
                assert_eq!(entries[0].depth, 0);
                assert_eq!(entries[0].tip_seq, None);
            }
            other => panic!("expected LineageEmit; got {other:?}"),
        }
    }

    #[test]
    fn lineage_back_walks_a_long_linear_chain_without_stack_overflow() {
        // Pin: pass-2 NEW-m4 flagged "very-large lineage chains:
        // largest test is 3 entries." Build a 500-deep linear
        // fork-of chain and walk back from the tail. The
        // planner's BFS uses `VecDeque::pop_front` and
        // `parent_of` is sort-based (not recursive), so this is
        // a depth-doesn't-overflow / time-bounded assertion.
        //
        // `parent_of` is O(nodes-in-index); BFS does O(depth)
        // steps; total is O(depth²). 500 is bounded enough to
        // run in single-digit seconds in dev profile while still
        // exercising depths well past anything a recursive
        // implementation would survive.
        const N: u64 = 500;
        let mut holders = Vec::with_capacity(N as usize);
        holders.push((1, caps_chain_only(1)));
        for i in 1..N {
            holders.push((i + 1, caps_chain_forked_from(i + 1, i)));
        }
        let (fold, _index) = index_with(holders);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(N),
                max_depth: (N + 10) as u32,
            }))
            .expect("10k-deep walk plans cleanly");
        match plan.root.operator {
            OperatorPlan::LineageEmit { entries, .. } => {
                assert_eq!(entries.len(), N as usize);
                // First entry is the tail; last is the root.
                assert_eq!(entries[0].origin, N);
                assert_eq!(entries[0].depth, 0);
                assert_eq!(entries[(N - 1) as usize].origin, 1);
                assert_eq!(entries[(N - 1) as usize].depth, (N - 1) as u32);
            }
            other => panic!("expected LineageEmit; got {other:?}"),
        }
    }

    #[test]
    fn lineage_forward_walks_a_wide_fanout_without_stack_overflow() {
        // Symmetric stress: a single root with 1k direct
        // children (no grandchildren). BFS over `children_of` +
        // lex sort. Pins the wide-fanout shape; the linear chain
        // pin above covers the deep shape.
        const N: u64 = 1_000;
        let root = 1;
        let mut holders = Vec::with_capacity((N + 1) as usize);
        holders.push((1, caps_chain_only(root)));
        for i in 0..N {
            let child = 2 + i;
            holders.push((100 + i, caps_chain_forked_from(child, root)));
        }
        let (fold, _index) = index_with(holders);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageForward {
                origin: ChainRef::OriginHash(root),
                max_depth: 5,
            }))
            .expect("1k-wide fan-out plans cleanly");
        match plan.root.operator {
            OperatorPlan::LineageEmit { entries, .. } => {
                // Root + N children.
                assert_eq!(entries.len(), (N + 1) as usize);
                assert_eq!(entries[0].origin, root);
                assert_eq!(entries[0].depth, 0);
                // Children all at depth 1.
                assert!(entries[1..].iter().all(|e| e.depth == 1));
            }
            other => panic!("expected LineageEmit; got {other:?}"),
        }
    }

    #[test]
    fn lineage_back_walks_through_three_generations() {
        // Grandparent (g) <- parent (p) <- child (c).
        let g = 0x0000_0000_0000_00AA_u64;
        let p = 0x0000_0000_0000_00BB_u64;
        let c = 0x0000_0000_0000_00CC_u64;
        let (fold, _index) = index_with(vec![
            (10, caps_chain_only(g)),
            (20, caps_chain_forked_from(p, g)),
            (30, caps_chain_forked_from(c, p)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(c),
                max_depth: 5,
            }))
            .unwrap();
        match plan.root.operator {
            OperatorPlan::LineageEmit { entries, .. } => {
                let chain: Vec<u64> = entries.iter().map(|e| e.origin).collect();
                assert_eq!(chain, vec![c, p, g]);
                let depths: Vec<u32> = entries.iter().map(|e| e.depth).collect();
                assert_eq!(depths, vec![0, 1, 2]);
            }
            other => panic!("expected LineageEmit; got {other:?}"),
        }
    }

    #[test]
    fn lineage_back_propagates_tip_seq_from_holders() {
        // Holder advertises chain + tip + fork-of: — tip
        // surfaces in the LineageEntry.
        let parent = 0xAA;
        let child = 0xBB;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_tip_forked_from(parent, 99, 0)), // root: fork-of:0 ignored
            (2, caps_chain_tip_forked_from(child, 42, parent)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(child),
                max_depth: 2,
            }))
            .unwrap();
        match plan.root.operator {
            OperatorPlan::LineageEmit { entries, .. } => {
                // child entry carries tip 42; parent entry
                // carries tip 99 (fork-of:0 from "root" doesn't
                // chain further since no host advertises chain 0).
                assert_eq!(entries[0].origin, child);
                assert_eq!(entries[0].tip_seq, Some(SeqNum(42)));
                assert_eq!(entries[1].origin, parent);
                assert_eq!(entries[1].tip_seq, Some(SeqNum(99)));
            }
            other => panic!("expected LineageEmit; got {other:?}"),
        }
    }

    #[test]
    fn lineage_back_detects_cycle() {
        // Pathological: A -> B -> A. Cycle should surface.
        let a = 0x000A;
        let b = 0x000B;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_forked_from(a, b)),
            (2, caps_chain_forked_from(b, a)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(a),
                max_depth: 10,
            }))
            .unwrap_err();
        match err {
            MeshError::LineageCycleDetected { origin, cycle } => {
                assert_eq!(origin, a);
                // Cycle must contain both a and b.
                assert!(cycle.contains(&a), "cycle missing a: {cycle:?}");
                assert!(cycle.contains(&b), "cycle missing b: {cycle:?}");
            }
            other => panic!("expected LineageCycleDetected; got {other:?}"),
        }
    }

    #[test]
    fn lineage_back_surfaces_max_depth_exceeded_when_walk_could_continue() {
        // 4-generation chain, max_depth=2: walk is truncated
        // and the planner surfaces the bound.
        let g0 = 0x10;
        let g1 = 0x11;
        let g2 = 0x12;
        let g3 = 0x13;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_only(g0)),
            (2, caps_chain_forked_from(g1, g0)),
            (3, caps_chain_forked_from(g2, g1)),
            (4, caps_chain_forked_from(g3, g2)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(g3),
                max_depth: 2,
            }))
            .unwrap_err();
        match err {
            MeshError::LineageMaxDepthExceeded { origin, depth } => {
                assert_eq!(origin, g3);
                assert_eq!(depth, 2);
            }
            other => panic!("expected LineageMaxDepthExceeded; got {other:?}"),
        }
    }

    #[test]
    fn lineage_back_terminates_exactly_at_max_depth_without_error() {
        // 3-generation chain, max_depth=2: walk is g2 -> g1 -> g0
        // and at depth 2 the parent_of g0 is None — no error.
        let g0 = 0x20;
        let g1 = 0x21;
        let g2 = 0x22;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_only(g0)),
            (2, caps_chain_forked_from(g1, g0)),
            (3, caps_chain_forked_from(g2, g1)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(g2),
                max_depth: 2,
            }))
            .unwrap();
        if let OperatorPlan::LineageEmit { entries, .. } = plan.root.operator {
            assert_eq!(
                entries.iter().map(|e| e.origin).collect::<Vec<_>>(),
                vec![g2, g1, g0]
            );
        } else {
            panic!("expected LineageEmit");
        }
    }

    #[test]
    fn lineage_forward_emits_descendants_bfs_sorted() {
        // Root has two children (c1 < c2 by hash). c1 has one
        // grandchild gc. BFS order: root, c1, c2, gc.
        let root = 0x100;
        let c1 = 0x110;
        let c2 = 0x120;
        let gc = 0x130;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_only(root)),
            (2, caps_chain_forked_from(c1, root)),
            (3, caps_chain_forked_from(c2, root)),
            (4, caps_chain_forked_from(gc, c1)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageForward {
                origin: ChainRef::OriginHash(root),
                max_depth: 5,
            }))
            .unwrap();
        match plan.root.operator {
            OperatorPlan::LineageEmit {
                direction, entries, ..
            } => {
                assert_eq!(direction, LineageDirection::Forward);
                let chain: Vec<u64> = entries.iter().map(|e| e.origin).collect();
                // BFS asc-depth, lex-sorted within a depth.
                assert_eq!(chain, vec![root, c1, c2, gc]);
                let depths: Vec<u32> = entries.iter().map(|e| e.depth).collect();
                assert_eq!(depths, vec![0, 1, 1, 2]);
            }
            other => panic!("expected LineageEmit; got {other:?}"),
        }
    }

    #[test]
    fn lineage_forward_surfaces_max_depth_when_descendants_remain() {
        // root -> c1 -> gc. max_depth=1: should surface bound
        // because gc is still reachable beyond.
        let root = 0x200;
        let c1 = 0x210;
        let gc = 0x220;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_only(root)),
            (2, caps_chain_forked_from(c1, root)),
            (3, caps_chain_forked_from(gc, c1)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&MeshQuery::V1(QueryV1::LineageForward {
                origin: ChainRef::OriginHash(root),
                max_depth: 1,
            }))
            .unwrap_err();
        match err {
            MeshError::LineageMaxDepthExceeded { origin, depth } => {
                assert_eq!(origin, root);
                assert_eq!(depth, 1);
            }
            other => panic!("expected LineageMaxDepthExceeded; got {other:?}"),
        }
    }

    #[test]
    fn lineage_forward_with_no_descendants_returns_only_start() {
        let leaf = 0x300;
        let (fold, _index) = index_with(vec![(1, caps_chain_only(leaf))]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageForward {
                origin: ChainRef::OriginHash(leaf),
                max_depth: 10,
            }))
            .unwrap();
        if let OperatorPlan::LineageEmit { entries, .. } = plan.root.operator {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].origin, leaf);
        } else {
            panic!("expected LineageEmit");
        }
    }

    #[test]
    fn lineage_back_with_max_depth_zero_returns_only_start_no_error() {
        // Regression: max_depth=0 = "just-the-origin". A present
        // parent must NOT trip LineageMaxDepthExceeded, because
        // the caller explicitly asked for zero steps.
        let g0 = 0x40;
        let g1 = 0x41;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_only(g0)),
            (2, caps_chain_forked_from(g1, g0)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(g1),
                max_depth: 0,
            }))
            .expect("max_depth=0 must succeed even when a parent exists");
        match plan.root.operator {
            OperatorPlan::LineageEmit { entries, .. } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].origin, g1);
                assert_eq!(entries[0].depth, 0);
            }
            other => panic!("expected LineageEmit; got {other:?}"),
        }
    }

    #[test]
    fn lineage_forward_with_max_depth_zero_returns_only_start_no_error() {
        // Same regression as above, forward variant. The start
        // node has a descendant; max_depth=0 says "don't descend".
        let parent = 0x50;
        let child = 0x51;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_only(parent)),
            (2, caps_chain_forked_from(child, parent)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageForward {
                origin: ChainRef::OriginHash(parent),
                max_depth: 0,
            }))
            .expect("max_depth=0 must succeed even when descendants exist");
        match plan.root.operator {
            OperatorPlan::LineageEmit { entries, .. } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].origin, parent);
                assert_eq!(entries[0].depth, 0);
            }
            other => panic!("expected LineageEmit; got {other:?}"),
        }
    }

    #[test]
    fn lineage_emit_round_trips_through_postcard() {
        // Pin the wire-encodability of LineageEmit so the
        // protocol layer can carry it inside an ExecutionPlan
        // without surprises.
        let parent = 0xAA;
        let child = 0xBB;
        let (fold, _index) = index_with(vec![
            (1, caps_chain_only(parent)),
            (2, caps_chain_forked_from(child, parent)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::LineageBack {
                origin: ChainRef::OriginHash(child),
                max_depth: 5,
            }))
            .unwrap();
        let bytes = postcard::to_allocvec(&plan).unwrap();
        let decoded: ExecutionPlan = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, plan);
    }

    // ========================================================================
    // Phase D — hash-join planning
    // ========================================================================

    fn join_query(
        left_field: &str,
        right_field: &str,
        left_chain: u64,
        right_chain: u64,
    ) -> MeshQuery {
        MeshQuery::V1(QueryV1::Join {
            left: Box::new(MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(left_chain),
            })),
            right: Box::new(MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(right_chain),
            })),
            on: JoinKey {
                left_field: Expr::Field(left_field.to_string()),
                right_field: Expr::Field(right_field.to_string()),
            },
            kind: JoinKind::Inner,
            watermark: Duration::from_secs(5),
        })
    }

    #[test]
    fn plan_join_on_origin_produces_hash_join_with_origin_key() {
        let l = 0x1111;
        let r = 0x2222;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_presence(l)),
            (2, caps_with_causal_presence(r)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner.plan(&join_query("origin", "origin", l, r)).unwrap();
        match plan.root.operator {
            OperatorPlan::HashJoin {
                key_mode,
                kind,
                strategy,
                watermark,
                left,
                right,
            } => {
                assert_eq!(key_mode, JoinKeyMode::Origin);
                assert_eq!(kind, JoinKind::Inner);
                assert_eq!(strategy, JoinStrategy::HashBroadcast);
                assert_eq!(watermark, Duration::from_secs(5));
                assert!(matches!(left.operator, OperatorPlan::LatestRead { .. }));
                assert!(matches!(right.operator, OperatorPlan::LatestRead { .. }));
            }
            other => panic!("expected HashJoin; got {other:?}"),
        }
    }

    #[test]
    fn plan_join_on_seq_produces_seq_key_mode() {
        let l = 0x3333;
        let r = 0x4444;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_presence(l)),
            (2, caps_with_causal_presence(r)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner.plan(&join_query("seq", "seq", l, r)).unwrap();
        if let OperatorPlan::HashJoin { key_mode, .. } = plan.root.operator {
            assert_eq!(key_mode, JoinKeyMode::Seq);
        } else {
            panic!("expected HashJoin");
        }
    }

    #[test]
    fn plan_join_with_mismatched_field_names_surfaces_planner_error() {
        let l = 0x5555;
        let r = 0x6666;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_presence(l)),
            (2, caps_with_causal_presence(r)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&join_query("origin", "seq", l, r))
            .unwrap_err();
        match err {
            MeshError::PlannerError { detail } => {
                assert!(detail.contains("same field name"), "got: {detail}");
            }
            other => panic!("expected PlannerError; got {other:?}"),
        }
    }

    #[test]
    fn plan_join_on_payload_field_produces_field_key_mode() {
        // Phase D-2 finish: payload-keyed joins resolve to
        // JoinKeyMode::Field(<path>), not a PlannerError.
        let l = 0x7777;
        let r = 0x8888;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_presence(l)),
            (2, caps_with_causal_presence(r)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&join_query(
                "payload.request_id",
                "payload.request_id",
                l,
                r,
            ))
            .unwrap();
        match plan.root.operator {
            OperatorPlan::HashJoin { key_mode, .. } => {
                assert_eq!(
                    key_mode,
                    JoinKeyMode::Field("payload.request_id".to_string())
                );
            }
            other => panic!("expected HashJoin; got {other:?}"),
        }
    }

    #[test]
    fn plan_join_with_non_field_expression_surfaces_planner_error() {
        let l = 0x9999;
        let r = 0xAAAA;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_presence(l)),
            (2, caps_with_causal_presence(r)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let q = MeshQuery::V1(QueryV1::Join {
            left: Box::new(MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(l),
            })),
            right: Box::new(MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(r),
            })),
            on: JoinKey {
                left_field: Expr::LitString("origin".to_string()),
                right_field: Expr::Field("origin".to_string()),
            },
            kind: JoinKind::Inner,
            watermark: Duration::from_secs(5),
        });
        let err = planner.plan(&q).unwrap_err();
        match err {
            MeshError::PlannerError { detail } => {
                assert!(detail.contains("field reference"));
            }
            other => panic!("expected PlannerError; got {other:?}"),
        }
    }

    #[test]
    fn plan_join_round_trips_through_postcard() {
        let l = 0xCCCC;
        let r = 0xDDDD;
        let (fold, _index) = index_with(vec![
            (1, caps_with_causal_presence(l)),
            (2, caps_with_causal_presence(r)),
        ]);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner.plan(&join_query("origin", "origin", l, r)).unwrap();
        let bytes = postcard::to_allocvec(&plan).unwrap();
        let decoded: ExecutionPlan = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, plan);
    }

    // ========================================================================
    // Phase E — aggregate planning (Count only in E-1)
    // ========================================================================

    fn aggregate_query(origin: u64, group_by: Vec<Expr>, agg_fn: AggregateFn) -> MeshQuery {
        MeshQuery::V1(QueryV1::Aggregate {
            inner: Box::new(MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            })),
            group_by,
            agg_fn,
        })
    }

    #[test]
    fn plan_aggregate_count_no_group_by() {
        let origin = 0x1111;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&aggregate_query(origin, vec![], AggregateFn::Count))
            .unwrap();
        match plan.root.operator {
            OperatorPlan::AggregateCount { input, group_by } => {
                assert_eq!(group_by, None);
                assert!(matches!(input.operator, OperatorPlan::LatestRead { .. }));
            }
            other => panic!("expected AggregateCount; got {other:?}"),
        }
    }

    #[test]
    fn plan_aggregate_count_group_by_origin() {
        let origin = 0x2222;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&aggregate_query(
                origin,
                vec![Expr::Field("origin".to_string())],
                AggregateFn::Count,
            ))
            .unwrap();
        if let OperatorPlan::AggregateCount { group_by, .. } = plan.root.operator {
            assert_eq!(group_by, Some(JoinKeyMode::Origin));
        } else {
            panic!("expected AggregateCount");
        }
    }

    #[test]
    fn plan_aggregate_count_group_by_seq() {
        let origin = 0x3333;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&aggregate_query(
                origin,
                vec![Expr::Field("seq".to_string())],
                AggregateFn::Count,
            ))
            .unwrap();
        if let OperatorPlan::AggregateCount { group_by, .. } = plan.root.operator {
            assert_eq!(group_by, Some(JoinKeyMode::Seq));
        } else {
            panic!("expected AggregateCount");
        }
    }

    #[test]
    fn plan_aggregate_count_group_by_origin_seq_composite() {
        let origin = 0x4444;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&aggregate_query(
                origin,
                vec![
                    Expr::Field("origin".to_string()),
                    Expr::Field("seq".to_string()),
                ],
                AggregateFn::Count,
            ))
            .unwrap();
        if let OperatorPlan::AggregateCount { group_by, .. } = plan.root.operator {
            assert_eq!(group_by, Some(JoinKeyMode::OriginSeq));
        } else {
            panic!("expected AggregateCount");
        }
    }

    #[test]
    fn plan_aggregate_sum_produces_aggregate_numeric() {
        let origin = 0x5555;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&aggregate_query(
                origin,
                vec![],
                AggregateFn::Sum {
                    field: Expr::Field("amount".to_string()),
                },
            ))
            .unwrap();
        match plan.root.operator {
            OperatorPlan::AggregateNumeric {
                group_by,
                field_path,
                kind,
                ..
            } => {
                assert_eq!(group_by, None);
                assert_eq!(field_path, "amount");
                assert_eq!(kind, super::super::query::NumericAggregateKind::Sum);
            }
            other => panic!("expected AggregateNumeric; got {other:?}"),
        }
    }

    #[test]
    fn plan_aggregate_avg_with_group_by_produces_aggregate_numeric() {
        let origin = 0x5556;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&aggregate_query(
                origin,
                vec![Expr::Field("origin".to_string())],
                AggregateFn::Avg {
                    field: Expr::Field("latency".to_string()),
                },
            ))
            .unwrap();
        if let OperatorPlan::AggregateNumeric {
            group_by,
            field_path,
            kind,
            ..
        } = plan.root.operator
        {
            assert_eq!(group_by, Some(JoinKeyMode::Origin));
            assert_eq!(field_path, "latency");
            assert_eq!(kind, super::super::query::NumericAggregateKind::Avg);
        } else {
            panic!("expected AggregateNumeric");
        }
    }

    #[test]
    fn plan_aggregate_non_field_arg_surfaces_planner_error() {
        let origin = 0x5557;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&aggregate_query(
                origin,
                vec![],
                AggregateFn::Sum {
                    field: Expr::LitInt(42),
                },
            ))
            .unwrap_err();
        match err {
            MeshError::PlannerError { detail } => {
                assert!(detail.contains("Expr::Field"), "got: {detail}");
            }
            other => panic!("expected PlannerError; got {other:?}"),
        }
    }

    #[test]
    fn plan_aggregate_sketch_function_still_surfaces_planner_error() {
        // HLL / T-Digest sketch implementations are deferred to
        // Phase F (or whenever a consumer's data volumes justify
        // them); exact equivalents ship via DistinctCountExact /
        // PercentileExact.
        let origin = 0x5558;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&aggregate_query(
                origin,
                vec![],
                AggregateFn::DistinctCountHll {
                    field: Expr::Field("user_id".to_string()),
                },
            ))
            .unwrap_err();
        match err {
            MeshError::PlannerError { detail } => {
                assert!(detail.contains("sketch implementation"), "got: {detail}");
                assert!(detail.contains("DistinctCountExact"), "got: {detail}");
            }
            other => panic!("expected PlannerError; got {other:?}"),
        }
    }

    #[test]
    fn plan_aggregate_group_by_payload_field_surfaces_planner_error() {
        let origin = 0x6666;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let err = planner
            .plan(&aggregate_query(
                origin,
                vec![Expr::Field("payload.severity".to_string())],
                AggregateFn::Count,
            ))
            .unwrap_err();
        match err {
            MeshError::PlannerError { detail } => {
                assert!(detail.contains("row-intrinsic"));
            }
            other => panic!("expected PlannerError; got {other:?}"),
        }
    }

    #[test]
    fn plan_aggregate_round_trips_through_postcard() {
        let origin = 0x7777;
        let (fold, _index) = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&fold,rtt_none);
        let plan = planner
            .plan(&aggregate_query(
                origin,
                vec![Expr::Field("origin".to_string())],
                AggregateFn::Count,
            ))
            .unwrap();
        let bytes = postcard::to_allocvec(&plan).unwrap();
        let decoded: ExecutionPlan = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, plan);
    }
}
