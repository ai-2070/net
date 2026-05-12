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
use super::query::{ChainRef, MeshQuery, QueryV1, SeqNum};
use crate::adapter::net::behavior::capability::CapabilityIndex;
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
    /// Capability index used for `Discovered` resolution + holder
    /// lookup.
    pub capability_index: &'a CapabilityIndex,
    /// RTT lookup closure. Same shape as
    /// `CapabilityQuery::nearest`.
    pub rtt_lookup: F,
}

impl<'a, F> MeshQueryPlanner<'a, F>
where
    F: Fn(u64) -> Option<Duration>,
{
    /// Construct a planner. Doesn't allocate.
    pub fn new(capability_index: &'a CapabilityIndex, rtt_lookup: F) -> Self {
        Self {
            capability_index,
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
            QueryV1::Join { left, right, .. } => {
                // Plan each side; emit a not-yet wrapper that
                // holds the left input. Phase D fills in the
                // executor.
                let _ = self.plan(right)?; // surface inner errors early
                let left_plan = self.plan(left)?;
                self.plan_not_yet_implemented("Join (Phase D)", Some(Box::new(left_plan.root)))
            }
            QueryV1::Aggregate { inner, .. } => {
                let input = self.plan(inner)?;
                self.plan_not_yet_implemented("Aggregate (Phase E)", Some(Box::new(input.root)))
            }
            QueryV1::Project { inner, .. } => {
                let input = self.plan(inner)?;
                self.plan_not_yet_implemented("Project (Phase A.2)", Some(Box::new(input.root)))
            }
            QueryV1::OrderBy { inner, .. } => {
                let input = self.plan(inner)?;
                self.plan_not_yet_implemented("OrderBy (Phase A.2)", Some(Box::new(input.root)))
            }
        }
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
        if self.parent_of(current).is_some() {
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
        let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
        visited.insert(start);
        let mut entries = vec![LineageEntry {
            origin: start,
            depth: 0,
            tip_seq: self.best_tip(start),
        }];
        let mut frontier: Vec<(u64, u32)> = vec![(start, 0)];
        while let Some((current, depth)) = frontier.first().copied() {
            frontier.remove(0);
            if depth >= max_depth {
                if !self.children_of(current).is_empty() {
                    return Err(MeshError::LineageMaxDepthExceeded {
                        origin: start,
                        depth: max_depth,
                    });
                }
                continue;
            }
            let mut children = self.children_of(current);
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
                frontier.push((child, depth + 1));
            }
        }
        Ok(entries)
    }

    /// Find the parent origin for `child` in the capability
    /// index. Scans every indexed node for the one that
    /// advertises `child` via `causal:<hex>` and reads its
    /// `fork-of:<parent_hex>` tag.
    ///
    /// Returns `None` when no node hosts `child` or the
    /// hosting node carries no fork-of declaration.
    /// Multi-chain hosts — a node with several `causal:` tags
    /// alongside several `fork-of:` tags — are a Phase C
    /// ambiguity: the first fork-of tag in iteration order
    /// wins.
    fn parent_of(&self, child: u64) -> Option<u64> {
        for node_id in self.capability_index.all_nodes() {
            let Some(caps) = self.capability_index.get(node_id) else {
                continue;
            };
            let mut hosts_child = false;
            let mut parent: Option<u64> = None;
            for tag in &caps.tags {
                let Tag::Reserved { prefix, body } = tag else {
                    continue;
                };
                match (prefix.as_str(), parent) {
                    ("causal:", _) if parse_causal_body(body) == Some(child) => {
                        hosts_child = true;
                    }
                    ("fork-of:", None) => {
                        parent = parse_fork_body(body);
                    }
                    _ => {}
                }
            }
            if hosts_child {
                return parent;
            }
        }
        None
    }

    /// Find all chains advertising `fork-of:<parent>` — i.e.,
    /// the direct descendants. Scans every node in the
    /// capability index; the result is sorted by caller (BFS
    /// needs deterministic order).
    fn children_of(&self, parent: u64) -> Vec<u64> {
        let mut out = Vec::new();
        for node_id in self.capability_index.all_nodes() {
            let Some(caps) = self.capability_index.get(node_id) else {
                continue;
            };
            let mut has_fork_to_parent = false;
            let mut owned_chain: Option<u64> = None;
            for tag in &caps.tags {
                let Tag::Reserved { prefix, body } = tag else {
                    continue;
                };
                match prefix.as_str() {
                    "fork-of:" if parse_fork_body(body) == Some(parent) => {
                        has_fork_to_parent = true;
                    }
                    "causal:" => {
                        // First causal tag wins (multi-chain
                        // hosts are a Phase C ambiguity).
                        if let Some(origin) = parse_causal_body(body) {
                            owned_chain.get_or_insert(origin);
                        }
                    }
                    _ => {}
                }
            }
            if has_fork_to_parent {
                if let Some(chain) = owned_chain {
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
    /// extract origin hashes. Returns the **first** origin
    /// hash (lex-sorted for determinism) when multiple match.
    /// Multi-origin discovery (implicit `Union` over all
    /// matched origins) lands in Phase B when the executor
    /// learns to fan out across chains.
    ///
    /// Errors:
    /// - `PlannerError` if the stored `PredicateWire` fails
    ///   to rebuild as a `Predicate`.
    /// - `NoCapableHolder` if the predicate matches zero
    ///   nodes OR every matched node's caps carry no
    ///   `causal:` tag.
    fn resolve_origin(&self, origin: &ChainRef) -> Result<u64, MeshError> {
        match origin {
            ChainRef::OriginHash(h) => Ok(*h),
            ChainRef::Discovered(wire) => {
                let predicate = wire.clone().into_predicate().map_err(|e| {
                    MeshError::PlannerError {
                        detail: format!("Discovered predicate rebuild failed: {e:?}"),
                    }
                })?;
                let candidates = self.capability_index.filter(&predicate);
                // Walk every matched node's caps + extract
                // origins from `causal:<hex>*` tags. Dedupe +
                // sort lex for determinism. Take the first.
                let mut origins: std::collections::BTreeSet<u64> =
                    std::collections::BTreeSet::new();
                for (_node_id, caps) in &candidates {
                    for tag in &caps.tags {
                        if let Some(hash) = parse_causal_origin(tag) {
                            origins.insert(hash);
                        }
                    }
                }
                origins
                    .into_iter()
                    .next()
                    .ok_or_else(|| MeshError::NoCapableHolder {
                        origin: 0,
                        requirement: format!("{:?}", predicate),
                    })
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
        for node_id in self.capability_index.all_nodes() {
            // Each node may advertise multiple `causal:`
            // variants for the same chain (presence + tip +
            // range during transitions). Pick the most
            // specific one — range > tip > presence — so
            // the planner gets the tightest claim.
            let claim = self
                .capability_index
                .with_caps(node_id, |caps| {
                    caps.tags
                        .iter()
                        .filter_map(|t| parse_causal_claim(t, &hex))
                        .max_by_key(specificity_rank)
                })
                .unwrap_or(None);
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
        OperatorPlan::NotYetImplemented {
            input: Some(input),
            ..
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
            Self::Tip { tip_seq } => {
                Some(SeqNum(0)..SeqNum(tip_seq.0.saturating_add(1)))
            }
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

/// Lowercase 16-char hex of a `u64` origin hash. Mirrors
/// `MeshNode::chain_hex` (which is private); duplicated
/// here so the planner doesn't depend on the mesh layer.
fn chain_hex(origin_hash: u64) -> String {
    format!("{origin_hash:016x}")
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

/// Parse a `causal:` body (everything after the `causal:`
/// prefix) into a `u64` origin hash. Strips the optional
/// `:<tip>` or `[start..end]` suffix before validating the
/// 16-hex-char stem.
fn parse_causal_body(body: &str) -> Option<u64> {
    let stem = body
        .split_once([':', '['])
        .map(|(s, _)| s)
        .unwrap_or(body);
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
    use crate::adapter::net::identity::EntityId;

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
        caps.tags.insert(causal_tag(format!(
            "{}:{}",
            chain_hex(origin_hash),
            tip
        )));
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

    fn index_with(holders: Vec<(u64, CapabilitySet)>) -> CapabilityIndex {
        let index = CapabilityIndex::new();
        for (node_id, caps) in holders {
            index.index(CapabilityAnnouncement::new(
                node_id,
                EntityId::from_bytes([node_id as u8; 32]),
                1,
                caps,
            ));
        }
        index
    }

    fn make_index_with_holder(node_id: u64, origin_hash: u64) -> CapabilityIndex {
        index_with(vec![(node_id, caps_with_causal_presence(origin_hash))])
    }

    fn empty_index() -> CapabilityIndex {
        CapabilityIndex::new()
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
        assert_eq!(claim, Some(CausalClaim::Tip { tip_seq: SeqNum(1000) }));
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

        let tip = CausalClaim::Tip { tip_seq: SeqNum(100) };
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

        let tip = CausalClaim::Tip { tip_seq: SeqNum(100) };
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
            (CausalClaim::Tip { tip_seq: SeqNum(99) }).advertised(),
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
            (CausalClaim::Tip { tip_seq: SeqNum(42) }).latest_tip(),
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
        assert_eq!(
            parse_causal_origin(&causal_tag("abc".to_string())),
            None
        );
    }

    // ========================================================================
    // Atomic-operator planning (At / Between / Latest)
    // ========================================================================

    #[test]
    fn plan_latest_returns_atomic_with_holder() {
        let origin = 0xABAB_ABAB_ABAB_ABAB_u64;
        let index = make_index_with_holder(42, origin);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = empty_index();
        let planner = MeshQueryPlanner::new(&index, rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(0),
            }))
            .expect("plan ok");
        assert!(plan.root.target_nodes.is_empty());
    }

    #[test]
    fn plan_between_rejects_inverted_range() {
        let index = empty_index();
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![(7, caps_with_causal_tip(origin, 1000))]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = make_index_with_holder(99, origin);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (200, caps.clone()),
            (50, caps.clone()),
            (100, caps),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (50, caps_with_causal_tip(origin, 50)),
            (200, caps_with_causal_tip(origin, 200)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_with_causal_range(origin, 0, 400)),
            (2, caps_with_causal_range(origin, 50, 600)),
            (3, caps_with_causal_tip(origin, 700)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_with_causal_range(origin, 0, 100)),
            (2, caps_with_causal_tip(origin, 50)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![(1, caps_with_causal_tip(origin, 50))]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![(1, caps_with_causal_presence(origin))]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_with_causal_tip(origin, 50)),
            (2, caps_with_causal_tip(origin, 500)),
            (3, caps_with_causal_tip(origin, 200)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (100, caps.clone()),
            (50, caps.clone()),
            (200, caps),
        ]);
        let rtt = |nid: u64| {
            Some(match nid {
                50 => Duration::from_millis(10),
                200 => Duration::from_millis(20),
                100 => Duration::from_millis(30),
                _ => return None::<Duration>,
            })
        };
        let planner = MeshQueryPlanner::new(&index, rtt);
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
        let index = index_with(vec![
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
        let planner = MeshQueryPlanner::new(&index, rtt);
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
        let index = index_with(vec![(7, caps)]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![(42, caps)]);
        let pred = Predicate::Exists {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            },
        };
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = empty_index();
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
    fn plan_chainref_discovered_match_with_no_causal_tag_surfaces_no_capable_holder() {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let caps = CapabilitySet::new().add_tag("dataforts.blob.storage");
        let index = index_with(vec![(7, caps)]);
        let pred = Predicate::Exists {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            },
        };
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let origin = 0x9999_9999_9999_9999_u64;
        let index = make_index_with_holder(1, origin);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
        let q = MeshQuery::V1(QueryV1::Aggregate {
            inner: Box::new(MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            })),
            group_by: vec![],
            agg_fn: super::super::query::AggregateFn::Count,
        });
        let plan = planner.plan(&q).unwrap();
        match plan.root.operator {
            OperatorPlan::NotYetImplemented { detail, input } => {
                assert!(detail.contains("Aggregate"));
                assert!(detail.contains("Phase E"));
                assert!(input.is_some(), "Aggregate's inner sub-plan must be carried");
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
        let index = make_index_with_holder(11, origin);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
    fn execution_plan_round_trips_through_postcard() {
        let origin = 0x1111_1111_1111_1111_u64;
        let index = make_index_with_holder(3, origin);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = make_index_with_holder(5, origin);
        let rtt = |nid: u64| {
            if nid == 5 {
                Some(Duration::from_millis(15))
            } else {
                None
            }
        };
        let planner = MeshQueryPlanner::new(&index, rtt);
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
        caps.tags.insert(causal_tag(format!(
            "{}:{}",
            chain_hex(chain),
            tip
        )));
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
        let index = index_with(vec![(1, caps_chain_only(root))]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
    fn lineage_back_walks_through_three_generations() {
        // Grandparent (g) <- parent (p) <- child (c).
        let g = 0x0000_0000_0000_00AA_u64;
        let p = 0x0000_0000_0000_00BB_u64;
        let c = 0x0000_0000_0000_00CC_u64;
        let index = index_with(vec![
            (10, caps_chain_only(g)),
            (20, caps_chain_forked_from(p, g)),
            (30, caps_chain_forked_from(c, p)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_chain_tip_forked_from(parent, 99, 0)), // root: fork-of:0 ignored
            (2, caps_chain_tip_forked_from(child, 42, parent)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_chain_forked_from(a, b)),
            (2, caps_chain_forked_from(b, a)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_chain_only(g0)),
            (2, caps_chain_forked_from(g1, g0)),
            (3, caps_chain_forked_from(g2, g1)),
            (4, caps_chain_forked_from(g3, g2)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_chain_only(g0)),
            (2, caps_chain_forked_from(g1, g0)),
            (3, caps_chain_forked_from(g2, g1)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_chain_only(root)),
            (2, caps_chain_forked_from(c1, root)),
            (3, caps_chain_forked_from(c2, root)),
            (4, caps_chain_forked_from(gc, c1)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![
            (1, caps_chain_only(root)),
            (2, caps_chain_forked_from(c1, root)),
            (3, caps_chain_forked_from(gc, c1)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
        let index = index_with(vec![(1, caps_chain_only(leaf))]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
    fn lineage_emit_round_trips_through_postcard() {
        // Pin the wire-encodability of LineageEmit so the
        // protocol layer can carry it inside an ExecutionPlan
        // without surprises.
        let parent = 0xAA;
        let child = 0xBB;
        let index = index_with(vec![
            (1, caps_chain_only(parent)),
            (2, caps_chain_forked_from(child, parent)),
        ]);
        let planner = MeshQueryPlanner::new(&index, rtt_none);
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
}
