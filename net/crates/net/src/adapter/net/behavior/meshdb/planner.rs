//! `MeshQueryPlanner` — translates a [`MeshQuery`] AST into an
//! [`ExecutionPlan`] tree the executor walks at run time.
//!
//! Phase A scope (this file): atomic operators (`At` / `Between`
//! / `Latest`) plan completely; composite operators
//! (`LineageBack`, `LineageForward`, `Join`, `Filter`,
//! `Aggregate`, `Project`, `OrderBy`) recurse into their inner
//! sub-queries, plan the inner, and surface a
//! `PlannerError { detail: "operator not yet implemented in
//! this build" }` if the executor for the outer operator hasn't
//! shipped yet. This lets downstream code (test fixtures,
//! cross-binding integration) start consuming the planner shape
//! today while the executor lands phase by phase.
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
        /// Resolved origin hash (post-`ChainRef::Discovered`
        /// resolution).
        origin: [u8; 32],
        /// Sequence number to read.
        seq: SeqNum,
    },
    /// Read events in `[start, end)` from one of the
    /// `target_nodes`. Half-open range.
    BetweenRead {
        /// Resolved origin hash.
        origin: [u8; 32],
        /// Lower bound (inclusive).
        start: SeqNum,
        /// Upper bound (exclusive).
        end: SeqNum,
    },
    /// Read the tip event from one of the `target_nodes`.
    LatestRead {
        /// Resolved origin hash.
        origin: [u8; 32],
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

            QueryV1::LineageBack { .. } => {
                self.plan_not_yet_implemented("LineageBack (Phase C)", None)
            }
            QueryV1::LineageForward { .. } => {
                self.plan_not_yet_implemented("LineageForward (Phase C)", None)
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
    /// origin via [`Self::resolve_origin`] then picks the
    /// proximity-nearest holder advertising
    /// `causal:<hex>:<tip_seq>` whose tip is `>= seq`. Phase A
    /// is permissive: any node carrying the chain qualifies;
    /// Phase B narrows to seq-range-covering holders.
    fn plan_at(&self, origin: &ChainRef, seq: SeqNum) -> Result<OperatorNode, MeshError> {
        let origin_hash = self.resolve_origin(origin)?;
        let targets = self.holders_of(&origin_hash);
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
    /// Phase A range gate: `start < end` required (otherwise
    /// surface `PlannerError`); the holder must advertise the
    /// chain. Phase B narrows to seq-range-aware routing.
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
        let targets = self.holders_of(&origin_hash);
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

    /// Plan an atomic `Latest(origin)` read.
    fn plan_latest(&self, origin: &ChainRef) -> Result<OperatorNode, MeshError> {
        let origin_hash = self.resolve_origin(origin)?;
        let targets = self.holders_of(&origin_hash);
        let cost = self.atomic_cost(&targets);
        Ok(OperatorNode {
            operator: OperatorPlan::LatestRead {
                origin: origin_hash,
            },
            target_nodes: targets,
            cost,
        })
    }

    /// Resolve a `ChainRef::OriginHash` or `ChainRef::Discovered`
    /// to a concrete 32-byte origin hash.
    ///
    /// For `Discovered`: rebuilds the typed `Predicate` from
    /// the stored `PredicateWire`, calls
    /// [`CapabilityQuery::filter`] on the capability index,
    /// then walks each matched node's `causal:<hex>` tags to
    /// extract origin hashes. Phase A semantics: returns the
    /// **first** origin hash (lex-sorted for determinism)
    /// when multiple match. Multi-origin discovery (implicit
    /// `Union` over all matched origins) lands in Phase B
    /// when the executor learns to fan out across chains.
    ///
    /// Errors:
    /// - `PlannerError` if the stored `PredicateWire` fails
    ///   to rebuild as a `Predicate`.
    /// - `NoCapableHolder` if the predicate matches zero
    ///   nodes OR every matched node's caps carry no
    ///   `causal:` tag.
    fn resolve_origin(&self, origin: &ChainRef) -> Result<[u8; 32], MeshError> {
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
                let mut origins: std::collections::BTreeSet<[u8; 32]> =
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
                        origin: [0; 32],
                        requirement: format!("{:?}", predicate),
                    })
            }
        }
    }

    /// Look up the set of `node_id`s holding `origin_hash`'s
    /// chain. Phase A heuristic: scans the capability index
    /// for `causal:<hex>*` reserved tags advertising the
    /// origin, returns the deduped list lexicographically
    /// sorted for determinism.
    fn holders_of(&self, origin_hash: &[u8; 32]) -> Vec<u64> {
        let hex = hex32(origin_hash);
        // Use the existing capability index's `find_first_host`
        // shape by scanning all nodes for a matching
        // `causal:<hex>` reserved-prefix tag. We can't use
        // `CapabilityQuery::match_axis` directly because
        // `causal:` is `Tag::Reserved`, not `Tag::AxisPresent` /
        // `AxisValue` — the typed axes don't cover it.
        let mut holders: Vec<u64> = self
            .capability_index
            .all_nodes()
            .into_iter()
            .filter(|nid| {
                self.capability_index
                    .with_caps(*nid, |caps| {
                        caps.tags.iter().any(|t| is_causal_for(t, &hex))
                    })
                    .unwrap_or(false)
            })
            .collect();
        // Sort for determinism. `nearest`-style proximity
        // ordering can layer on top in Phase A.3 once we wire
        // the rtt_lookup; Phase A here ships the deterministic
        // baseline.
        holders.sort_unstable();
        holders
    }

    /// Cost-estimate stub for atomic operators (Phase A).
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
        // Atomic operators + leaf `NotYetImplemented` —
        // no children to sum.
        OperatorPlan::AtRead { .. }
        | OperatorPlan::BetweenRead { .. }
        | OperatorPlan::LatestRead { .. }
        | OperatorPlan::NotYetImplemented { input: None, .. } => {}
    }
    acc
}

/// Lowercase hex of a 32-byte origin hash. Matches the
/// `chain_hex` convention used throughout the substrate.
fn hex32(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Is `tag` a `causal:<hex>*` reserved tag for the supplied
/// origin-hash hex string? Mirrors `MeshNode::is_causal_for`
/// (which is private); duplicated here so the planner doesn't
/// depend on the mesh layer.
fn is_causal_for(tag: &Tag, origin_hex: &str) -> bool {
    if let Tag::Reserved { prefix, body } = tag {
        if prefix != "causal:" {
            return false;
        }
        // `<hex>` exact match, or `<hex>:<tip_seq>`, or
        // `<hex>[start..end]` — match the prefix up to the
        // first `:` / `[` / end-of-string.
        let stem = body
            .split_once([':', '['])
            .map(|(s, _)| s)
            .unwrap_or(body.as_str());
        stem == origin_hex
    } else {
        false
    }
}

/// Extract the 32-byte origin hash from a `causal:<hex>*`
/// reserved tag. Returns `None` if the tag isn't a `causal:`
/// reserved tag, the body's stem isn't 64 hex chars, or any
/// nibble fails to parse.
///
/// Used by `ChainRef::Discovered` resolution to map every
/// matched node's caps to its set of advertised origin hashes.
fn parse_causal_origin(tag: &Tag) -> Option<[u8; 32]> {
    let Tag::Reserved { prefix, body } = tag else {
        return None;
    };
    if prefix != "causal:" {
        return None;
    }
    let stem = body
        .split_once([':', '['])
        .map(|(s, _)| s)
        .unwrap_or(body.as_str());
    if stem.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = stem.get(i * 2..i * 2 + 2)?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

// Silence unused-import warning when no operator uses the
// imported types in a particular planner-build configuration.
#[allow(dead_code)]
const _PLANNER_USES_TAXONOMY_AXIS: TaxonomyAxis = TaxonomyAxis::Dataforts;
#[allow(dead_code)]
fn _planner_uses_capability_query<Q: CapabilityQuery>(_q: &Q) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilityIndex, CapabilitySet};
    use crate::adapter::net::identity::EntityId;

    /// Build a fresh capability set carrying a single
    /// `causal:<hex>` reserved tag for `origin_hash`. The
    /// public `add_tag` builder silently drops reserved-
    /// prefix tags (it routes through `Tag::parse_user`), so
    /// the tests build the tag directly via `Tag::Reserved`
    /// to mimic what `MeshNode::announce_chain` emits.
    fn caps_with_causal(origin_hash: &[u8; 32]) -> CapabilitySet {
        let hex = hex32(origin_hash);
        let mut caps = CapabilitySet::new();
        caps.tags.insert(Tag::Reserved {
            prefix: "causal:".to_string(),
            body: hex,
        });
        caps
    }

    fn make_index_with_holder(node_id: u64, origin_hash: &[u8; 32]) -> CapabilityIndex {
        let caps = caps_with_causal(origin_hash);
        let index = CapabilityIndex::new();
        index.index(CapabilityAnnouncement::new(
            node_id,
            EntityId::from_bytes([0x11; 32]),
            1,
            caps,
        ));
        index
    }

    fn empty_index() -> CapabilityIndex {
        CapabilityIndex::new()
    }

    fn rtt_none(_nid: u64) -> Option<Duration> {
        None
    }

    #[test]
    fn plan_latest_returns_atomic_with_holder() {
        let origin = [0xAB; 32];
        let index = make_index_with_holder(42, &origin);
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
        // Phase A: the planner doesn't fail here — it produces
        // a plan with an empty target list. The executor
        // surfaces `HistoricalRangeUnavailable` when the plan
        // runs against the empty target set. This split lets
        // the test suite plan against empty indices.
        let index = empty_index();
        let planner = MeshQueryPlanner::new(&index, rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash([0; 32]),
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
                origin: ChainRef::OriginHash([0; 32]),
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
    fn plan_between_accepts_valid_range() {
        let origin = [0x42; 32];
        let index = make_index_with_holder(7, &origin);
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
    }

    #[test]
    fn plan_at_routes_to_holder() {
        let origin = [0xCC; 32];
        let index = make_index_with_holder(99, &origin);
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
    fn plan_holders_sorted_for_determinism() {
        // Two holders for the same chain — `holders_of` must
        // return them in lexicographic node_id order so the
        // plan is deterministic across runs.
        let origin = [0xEE; 32];
        let caps = caps_with_causal(&origin);
        let index = CapabilityIndex::new();
        // Insert in non-monotonic order to prove the sort
        // happens inside `holders_of`.
        for nid in [200u64, 50, 100] {
            index.index(CapabilityAnnouncement::new(
                nid,
                EntityId::from_bytes([nid as u8; 32]),
                1,
                caps.clone(),
            ));
        }
        let planner = MeshQueryPlanner::new(&index, rtt_none);
        let plan = planner
            .plan(&MeshQuery::V1(QueryV1::Latest {
                origin: ChainRef::OriginHash(origin),
            }))
            .unwrap();
        assert_eq!(plan.root.target_nodes, vec![50, 100, 200]);
    }

    #[test]
    fn plan_chainref_discovered_resolves_via_filter() {
        // The capability-system survey confirmed `filter` is the
        // primitive Discovered resolution leans on. Set up a
        // node carrying both a `dataforts.blob.storage` axis tag
        // (matched by the predicate) AND a `causal:<hex>` reserved
        // tag (extracted as the origin hash); the planner should
        // resolve the predicate to the matching origin.
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let origin = [0xCA; 32];
        let hex = hex32(&origin);
        let mut caps = CapabilitySet::new()
            .add_tag("dataforts.blob.storage");
        caps.tags.insert(Tag::Reserved {
            prefix: "causal:".to_string(),
            body: hex,
        });
        let index = CapabilityIndex::new();
        index.index(CapabilityAnnouncement::new(
            42,
            EntityId::from_bytes([0x33; 32]),
            1,
            caps,
        ));

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
        // No node advertises `dataforts.blob.storage` → the
        // predicate matches zero candidates → planner surfaces
        // `NoCapableHolder` with the rendered requirement.
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
                assert!(
                    requirement.contains("Exists"),
                    "requirement should render the predicate; got {requirement:?}"
                );
            }
            other => panic!("expected NoCapableHolder; got {other:?}"),
        }
    }

    #[test]
    fn plan_chainref_discovered_match_with_no_causal_tag_surfaces_no_capable_holder() {
        // Node matches the predicate but carries no
        // `causal:<hex>` tag (the chain hasn't been advertised
        // yet). Planner can't extract an origin → surfaces
        // `NoCapableHolder` rather than fabricating.
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let caps = CapabilitySet::new().add_tag("dataforts.blob.storage");
        let index = CapabilityIndex::new();
        index.index(CapabilityAnnouncement::new(
            7,
            EntityId::from_bytes([0x44; 32]),
            1,
            caps,
        ));
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

    #[test]
    fn parse_causal_origin_round_trips_hex() {
        // Helper round-trip: build a `causal:<hex>` tag from
        // a known origin, parse it back, assert equality.
        let origin = [0xDE; 32];
        let hex = hex32(&origin);
        let tag = Tag::Reserved {
            prefix: "causal:".to_string(),
            body: hex,
        };
        assert_eq!(parse_causal_origin(&tag), Some(origin));

        // With a tip suffix.
        let tag_with_tip = Tag::Reserved {
            prefix: "causal:".to_string(),
            body: format!("{}:42", hex32(&origin)),
        };
        assert_eq!(parse_causal_origin(&tag_with_tip), Some(origin));

        // Non-causal tag returns None.
        let not_causal = Tag::Reserved {
            prefix: "heat:".to_string(),
            body: hex32(&origin),
        };
        assert_eq!(parse_causal_origin(&not_causal), None);

        // Wrong-length hex returns None.
        let bad_hex = Tag::Reserved {
            prefix: "causal:".to_string(),
            body: "abc".to_string(),
        };
        assert_eq!(parse_causal_origin(&bad_hex), None);
    }

    #[test]
    fn plan_composite_operator_surfaces_not_yet_implemented() {
        // Phase A.1 plans atomic operators end-to-end; composite
        // operators emit a `NotYetImplemented` wrapper that
        // names the phase they ship in.
        let origin = [0x99; 32];
        let index = make_index_with_holder(1, &origin);
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
        // Same query + same index → same plan. Repeated calls
        // must produce byte-identical encoded plans (the
        // locked-decision-#4 cache key depends on this).
        let origin = [0x55; 32];
        let index = make_index_with_holder(11, &origin);
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
        let origin = [0x11; 32];
        let index = make_index_with_holder(3, &origin);
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
        let origin = [0x22; 32];
        let index = make_index_with_holder(5, &origin);
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
}
