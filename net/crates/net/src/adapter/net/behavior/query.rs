//! Federated query primitives over the capability index.
//!
//! Phase E of `CAPABILITY_SYSTEM_PLAN.md`. The plan calls for
//! five composable operators — `filter`, `match_axis`, `traverse`,
//! `aggregate`, `nearest` — that decompose dual-axis cross-axis
//! queries into compositions of primitives. This slice ships
//! the trait + reference impl for the first three; `traverse`
//! and `nearest` follow in slice 2 once their substrate
//! contracts (edge-kind taxonomy + proximity lookup) are
//! scoped.
//!
//! ## Composability
//!
//! Operators chain. The user-facing query a downstream consumer
//! (Rebel Yell, Atomic Playboys) might write —
//!
//! ```text
//! hardware.gpu AND software.model:llama-3-70b AND dataforts.has_chain:Y
//! ```
//!
//! — decomposes to:
//!
//! ```ignore
//! let candidates: Vec<(NodeId, _)> = index
//!     .match_axis(TaxonomyAxis::Hardware, "gpu", None)
//!     .filter(|(_, caps)| {
//!         let owned: Vec<Tag> = caps.tags.iter().cloned().collect();
//!         let model_pred = Predicate::Equals {
//!             key: TagKey::new(TaxonomyAxis::Software, "model".to_string()),
//!             value: "llama-3-70b".to_string(),
//!         };
//!         model_pred.evaluate_unplanned(&EvalContext::new(&owned, &caps.metadata))
//!     })
//!     .collect();
//! ```
//!
//! No new operators needed for the cross-axis composition.
//!
//! ## Local-only
//!
//! All operators run against the **local** capability-index view
//! by default. Cross-node federation is opt-in via a
//! `Federated` wrapper trait (Phase E slice 3, not in this
//! slice). Local-first avoids surprising round-trips on every
//! call; downstream code that needs federation reaches for the
//! wrapper deliberately.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::adapter::net::behavior::{
    capability::CapabilityIndex, predicate::EvalContext, predicate::Predicate, tag::Tag,
    tag::TagKey, tag::TaxonomyAxis,
};

/// Reserved-prefix edge kind for [`CapabilityQuery::traverse`].
/// Identifies which reserved-prefix tag forms a graph edge from a
/// child entity to its parent.
///
/// Today's substrate uses two reserved-prefix shapes that
/// genuinely encode parent links:
///
/// - `fork-of:<parent_origin_hex>` — a forked entity carries a
///   `fork-of:` tag whose body is the parent's origin hash. The
///   parent itself may carry its own `fork-of:` tag for the
///   grand-parent. Walking these chains terminates at a root
///   (an entity with no `fork-of:` tag).
///
/// - `causal:<chain_hex>` is NOT an edge kind in the
///   parent-pointer sense — it's a chain advertisement. Listed
///   here so adding it later (e.g., for "find the chain head of
///   chain X" traversal) is mechanical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Walk `fork-of:<parent>` parent links upward.
    ForkOfParent,
}

impl EdgeKind {
    /// Reserved-prefix string this edge kind walks. Matches the
    /// `Tag::Reserved::prefix` field for the corresponding tag.
    pub fn prefix(&self) -> &'static str {
        match self {
            EdgeKind::ForkOfParent => "fork-of:",
        }
    }
}

/// Proximity distance for [`CapabilityQuery::nearest`]. Wraps
/// `Duration` so callers can't accidentally swap it with a wall-
/// clock duration. RTT "missing" (e.g. no proximity data for a
/// candidate) is represented as `None` at the lookup boundary;
/// `nearest` ranks unmeasured candidates at the back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Distance(pub Duration);

/// Aggregator over filtered query results. Implementations
/// receive each matching `(NodeId, &CapabilitySet)` and update
/// internal state; `finalize` produces the typed output.
///
/// Three reference impls ship in this module: [`Count`],
/// [`UniqueAxisValues`], and [`MaxNumericMetadata`]. Plan §6
/// calls these "Aggregator" — same name retained.
///
/// Custom aggregators (per-binding folds) can layer on top of
/// this trait without substrate changes; matches the plan's
/// "no fold required for capability-level aggregates" decision.
pub trait Aggregator {
    /// Final output type once aggregation completes.
    type Output;

    /// Update internal state with a matching candidate.
    fn observe(
        &mut self,
        node_id: u64,
        caps: &crate::adapter::net::behavior::capability::CapabilitySet,
    );

    /// Produce the final aggregated value.
    fn finalize(self) -> Self::Output;
}

/// Reference impl: count matching candidates.
#[derive(Default)]
pub struct Count(usize);

impl Aggregator for Count {
    type Output = usize;
    fn observe(
        &mut self,
        _: u64,
        _: &crate::adapter::net::behavior::capability::CapabilitySet,
    ) {
        self.0 += 1;
    }
    fn finalize(self) -> usize {
        self.0
    }
}

/// Reference impl: collect the distinct values present under a
/// specific axis-key across matching candidates. Useful for
/// "what tenant ids are running GPU workloads?" style queries.
pub struct UniqueAxisValues {
    axis: TaxonomyAxis,
    key: String,
    seen: std::collections::BTreeSet<String>,
}

impl UniqueAxisValues {
    /// Build an aggregator for `axis.key`. The aggregator
    /// observes each matching node's tag set, extracts any tag
    /// of shape `axis.key=value`, and accumulates the distinct
    /// `value`s.
    pub fn new(axis: TaxonomyAxis, key: impl Into<String>) -> Self {
        Self {
            axis,
            key: key.into(),
            seen: std::collections::BTreeSet::new(),
        }
    }
}

impl Aggregator for UniqueAxisValues {
    type Output = Vec<String>;

    fn observe(
        &mut self,
        _: u64,
        caps: &crate::adapter::net::behavior::capability::CapabilitySet,
    ) {
        for tag in &caps.tags {
            if let Tag::AxisValue { axis, key, value, .. } = tag {
                if *axis == self.axis && key == &self.key {
                    self.seen.insert(value.clone());
                }
            }
        }
    }

    fn finalize(self) -> Vec<String> {
        self.seen.into_iter().collect()
    }
}

/// Reference impl: take the maximum of a numeric metadata field
/// across matching candidates. Returns `None` when no
/// candidate's metadata carries the key OR no value parses as a
/// finite `f64`.
pub struct MaxNumericMetadata {
    key: String,
    current_max: Option<f64>,
}

impl MaxNumericMetadata {
    /// Build an aggregator over `metadata[key]`. Values are
    /// parsed via `f64::from_str` — non-numeric values are
    /// ignored rather than panicking.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            current_max: None,
        }
    }
}

impl Aggregator for MaxNumericMetadata {
    type Output = Option<f64>;

    fn observe(
        &mut self,
        _: u64,
        caps: &crate::adapter::net::behavior::capability::CapabilitySet,
    ) {
        let Some(raw) = caps.metadata.get(&self.key) else {
            return;
        };
        let Ok(parsed) = raw.parse::<f64>() else {
            return;
        };
        if !parsed.is_finite() {
            return;
        }
        self.current_max = Some(match self.current_max {
            Some(m) if m >= parsed => m,
            _ => parsed,
        });
    }

    fn finalize(self) -> Option<f64> {
        self.current_max
    }
}

/// Five composable operators over the capability index. This
/// slice ships `filter`, `match_axis`, and `aggregate`;
/// `traverse` and `nearest` land in slice 2 once their
/// edge-kind / proximity-lookup contracts are scoped.
///
/// Reference impl on [`CapabilityIndex`]; downstream consumers
/// can layer their own (federated, in-memory test fixtures, …)
/// without changing the trait surface.
pub trait CapabilityQuery {
    /// Scan the index, returning `(node_id, caps)` pairs whose
    /// tags + metadata satisfy `predicate`. Ordering is
    /// implementation-defined; callers that need stable order
    /// sort the result.
    fn filter(
        &self,
        predicate: &Predicate,
    ) -> Vec<(u64, crate::adapter::net::behavior::capability::CapabilitySet)>;

    /// Type-aware match against a single axis-key. `value =
    /// None` matches any candidate carrying the axis-key (in
    /// either `axis.key` presence form or `axis.key=value` form);
    /// `value = Some(s)` requires exact value match against the
    /// `axis.key=value` form.
    ///
    /// Cheaper than `filter(Equals/Exists)` — skips the
    /// predicate AST + the `EvalContext` materialization, walks
    /// each node's tag set once.
    fn match_axis(
        &self,
        axis: TaxonomyAxis,
        key: &str,
        value: Option<&str>,
    ) -> Vec<(u64, crate::adapter::net::behavior::capability::CapabilitySet)>;

    /// Run `agg` across every candidate satisfying `predicate`.
    /// Streaming: each match's caps are observed in turn; no
    /// intermediate `Vec` allocation. `Aggregator::finalize`
    /// produces the typed output.
    fn aggregate<A>(&self, predicate: &Predicate, agg: A) -> A::Output
    where
        A: Aggregator;

    /// Walk capability-tag edges recursively. Starting from
    /// `start_tag`, follow [`EdgeKind`]-shaped parent links up
    /// to `max_depth` hops. Returns the chain of
    /// `(node_id, tag)` pairs visited, in walk order.
    ///
    /// For `EdgeKind::ForkOfParent`: `start_tag` is a
    /// `fork-of:<parent_origin_hex>` tag. The walker:
    ///
    ///   1. Records the start node + tag.
    ///   2. Looks up nodes hosting `causal:<parent_origin_hex>`
    ///      (the parent chain's holders).
    ///   3. From each holder, reads any `fork-of:` tag — the
    ///      grandparent — and recurses.
    ///   4. Terminates at `max_depth` hops OR when no
    ///      `fork-of:` tag is found (root reached).
    ///
    /// First parent's host wins on ties — the index iteration
    /// order is implementation-defined; callers needing
    /// deterministic ordering across runs should snapshot the
    /// index outside this call.
    ///
    /// `max_depth = 0` returns just `(start_node, start_tag)`
    /// without recursing. `start_node` defaults to `0` if the
    /// caller doesn't have a node id (the start tag itself is
    /// what matters for the walk).
    fn traverse(
        &self,
        start_node: u64,
        start_tag: &Tag,
        edge: EdgeKind,
        max_depth: u32,
    ) -> Vec<(u64, Tag)>;

    /// Top-N candidates by proximity. Filters by `predicate`,
    /// then ranks survivors by `rtt_lookup(node_id)` (lower
    /// RTT first). Candidates with no RTT data sort to the
    /// back; ties broken by lex-NodeId for determinism.
    ///
    /// `n = 0` returns empty. The function clones the matching
    /// `CapabilitySet`s into the result; callers that don't
    /// need the caps for downstream work can use `filter` +
    /// their own ranking.
    ///
    /// Distance closure decouples `nearest` from the substrate's
    /// proximity-graph internals (which use a `[u8; 32]`
    /// node-id shape that this trait would otherwise have to
    /// know about). Same plumbing convention as Phase F's
    /// `RttLookup` for placement scoring.
    fn nearest<F: Fn(u64) -> Option<Duration>>(
        &self,
        predicate: &Predicate,
        rtt_lookup: F,
        n: usize,
    ) -> Vec<(
        u64,
        crate::adapter::net::behavior::capability::CapabilitySet,
        Option<Distance>,
    )>;
}

// =========================================================================
// Reference impl over CapabilityIndex
// =========================================================================

impl CapabilityQuery for CapabilityIndex {
    fn filter(
        &self,
        predicate: &Predicate,
    ) -> Vec<(u64, crate::adapter::net::behavior::capability::CapabilitySet)> {
        let mut out = Vec::new();
        for node_id in self.all_nodes() {
            // `with_caps` keeps the read-lock scope tight — we
            // build the EvalContext under the lock, run the
            // predicate, and only clone the caps if it matches.
            let matched: Option<
                crate::adapter::net::behavior::capability::CapabilitySet,
            > = self.with_caps(node_id, |caps| {
                let owned_tags: Vec<Tag> = caps.tags.iter().cloned().collect();
                let ctx = EvalContext::new(&owned_tags, &caps.metadata);
                if predicate.evaluate_unplanned(&ctx) {
                    Some(caps.clone())
                } else {
                    None
                }
            }).flatten();
            if let Some(caps) = matched {
                out.push((node_id, caps));
            }
        }
        out
    }

    fn match_axis(
        &self,
        axis: TaxonomyAxis,
        key: &str,
        value: Option<&str>,
    ) -> Vec<(u64, crate::adapter::net::behavior::capability::CapabilitySet)> {
        let mut out = Vec::new();
        for node_id in self.all_nodes() {
            let matched: Option<
                crate::adapter::net::behavior::capability::CapabilitySet,
            > = self.with_caps(node_id, |caps| {
                if axis_match(caps, axis, key, value) {
                    Some(caps.clone())
                } else {
                    None
                }
            }).flatten();
            if let Some(caps) = matched {
                out.push((node_id, caps));
            }
        }
        out
    }

    fn aggregate<A>(&self, predicate: &Predicate, mut agg: A) -> A::Output
    where
        A: Aggregator,
    {
        for node_id in self.all_nodes() {
            self.with_caps(node_id, |caps| {
                let owned_tags: Vec<Tag> = caps.tags.iter().cloned().collect();
                let ctx = EvalContext::new(&owned_tags, &caps.metadata);
                if predicate.evaluate_unplanned(&ctx) {
                    agg.observe(node_id, caps);
                }
            });
        }
        agg.finalize()
    }

    fn traverse(
        &self,
        start_node: u64,
        start_tag: &Tag,
        edge: EdgeKind,
        max_depth: u32,
    ) -> Vec<(u64, Tag)> {
        let mut path = vec![(start_node, start_tag.clone())];
        if max_depth == 0 {
            return path;
        }

        let mut current_tag = start_tag.clone();
        for _ in 0..max_depth {
            // Extract the `(prefix, body)` of the current edge tag.
            // ForkOfParent walks `fork-of:<hex>` whose body is the
            // parent's origin hash; the parent is hosted on nodes
            // carrying `causal:<hex>`.
            let body = match &current_tag {
                Tag::Reserved { prefix, body } if prefix == edge.prefix() => body.clone(),
                _ => break, // current tag isn't the right edge kind; halt.
            };

            // Find a node hosting the parent (carries `causal:<body>`).
            let parent_lookup_tag = Tag::Reserved {
                prefix: "causal:".to_string(),
                body: body.clone(),
            };
            let parent_host = self.find_first_host(&parent_lookup_tag);
            let Some(parent_node) = parent_host else {
                break; // no host of the parent chain known to this index.
            };

            // Record the parent host, then look at its `fork-of:`
            // tag (if any) to continue walking up.
            let next_edge: Option<Tag> = self
                .with_caps(parent_node, |caps| {
                    caps.tags.iter().find_map(|t| match t {
                        Tag::Reserved { prefix, .. } if prefix == edge.prefix() => {
                            Some(t.clone())
                        }
                        _ => None,
                    })
                })
                .flatten();

            path.push((parent_node, parent_lookup_tag));
            match next_edge {
                Some(t) => current_tag = t,
                None => break, // parent has no further parent — root reached.
            }
        }
        path
    }

    fn nearest<F: Fn(u64) -> Option<Duration>>(
        &self,
        predicate: &Predicate,
        rtt_lookup: F,
        n: usize,
    ) -> Vec<(
        u64,
        crate::adapter::net::behavior::capability::CapabilitySet,
        Option<Distance>,
    )> {
        if n == 0 {
            return Vec::new();
        }
        // Gather (node_id, caps) survivors.
        let mut survivors = self.filter(predicate);
        // Compute RTTs.
        let mut ranked: Vec<(
            u64,
            crate::adapter::net::behavior::capability::CapabilitySet,
            Option<Distance>,
        )> = survivors
            .drain(..)
            .map(|(id, caps)| {
                let dist = rtt_lookup(id).map(Distance);
                (id, caps, dist)
            })
            .collect();
        // Sort: present RTTs ascending, missing RTTs last; ties
        // broken by lex NodeId for determinism.
        ranked.sort_by(|a, b| match (a.2, b.2) {
            (Some(da), Some(db)) => da.cmp(&db).then_with(|| a.0.cmp(&b.0)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0),
        });
        ranked.truncate(n);
        ranked
    }
}

impl CapabilityIndex {
    /// Find a node hosting `tag` — first match in iteration
    /// order. Used by `traverse` to resolve `causal:<hex>` →
    /// hosting node. The "first match" policy means the result
    /// isn't deterministic across runs unless callers control
    /// the index population order; the trait doc-comment on
    /// `traverse` calls this out.
    fn find_first_host(&self, tag: &Tag) -> Option<u64> {
        for node_id in self.all_nodes() {
            let hit = self
                .with_caps(node_id, |caps| caps.tags.contains(tag))
                .unwrap_or(false);
            if hit {
                return Some(node_id);
            }
        }
        None
    }
}

// =========================================================================
// Helpers
// =========================================================================

/// Walk `caps.tags` looking for `axis.key` (`value=None` →
/// presence-OR-value-form match; `value=Some(s)` → exact
/// `axis.key=s` value-form match). Cheaper than building an
/// EvalContext + running a Predicate AST since we walk the tag
/// set once with a tight `match`.
fn axis_match(
    caps: &crate::adapter::net::behavior::capability::CapabilitySet,
    axis: TaxonomyAxis,
    key: &str,
    value: Option<&str>,
) -> bool {
    for tag in &caps.tags {
        match tag {
            Tag::AxisPresent { axis: tag_axis, key: tag_key } => {
                if *tag_axis == axis && tag_key == key && value.is_none() {
                    return true;
                }
            }
            Tag::AxisValue { axis: tag_axis, key: tag_key, value: tag_value, .. } => {
                if *tag_axis != axis || tag_key != key {
                    continue;
                }
                match value {
                    None => return true,                  // any value satisfies presence
                    Some(target) if tag_value == target => return true,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    false
}

// `BTreeMap` is referenced in the inline doc-comment example at
// the top of the module; suppress unused-import warnings while
// keeping the docs link-checkable.
#[allow(dead_code)]
const _DOC_LINK: BTreeMap<String, String> = BTreeMap::new();
// `TagKey` referenced in the doc-comment example.
#[allow(dead_code)]
fn _doc_link_tag_key(axis: TaxonomyAxis, k: &str) -> TagKey {
    TagKey::new(axis, k.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        CapabilityAnnouncement, CapabilityIndex, CapabilitySet,
    };
    use crate::adapter::net::identity::EntityId;
    use std::sync::Arc;

    fn idx() -> Arc<CapabilityIndex> {
        let i = Arc::new(CapabilityIndex::new());
        let eid = EntityId::from_bytes([0u8; 32]);
        let nodes = [
            (
                0x1111u64,
                CapabilitySet::default()
                    .add_tag("hardware.gpu")
                    .add_tag("hardware.memory_mb=65536")
                    .with_metadata("region", "us-east")
                    .with_metadata("intent", "ml-training"),
            ),
            (
                0x2222,
                CapabilitySet::default()
                    .add_tag("hardware.cpu_cores=64")
                    .add_tag("hardware.memory_mb=32768")
                    .with_metadata("region", "us-east"),
            ),
            (
                0x3333,
                CapabilitySet::default()
                    .add_tag("hardware.gpu")
                    .add_tag("hardware.memory_mb=16384")
                    .with_metadata("region", "us-west")
                    .with_metadata("intent", "ml-training"),
            ),
            (
                0x4444,
                CapabilitySet::default()
                    .add_tag("hardware.cpu_cores=16")
                    .with_metadata("region", "eu-central"),
            ),
        ];
        for (id, caps) in nodes {
            i.index(CapabilityAnnouncement::new(id, eid.clone(), 1, caps));
        }
        i
    }

    /// `match_axis` finds GPU-tagged nodes regardless of value
    /// form (presence tag).
    #[test]
    fn match_axis_presence_finds_all_gpu_nodes() {
        let i = idx();
        let mut got: Vec<u64> = i
            .match_axis(TaxonomyAxis::Hardware, "gpu", None)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        got.sort();
        assert_eq!(got, vec![0x1111, 0x3333]);
    }

    /// `match_axis` with explicit value matches the value-form
    /// tag exactly.
    #[test]
    fn match_axis_with_value_matches_only_exact() {
        let i = idx();
        let mut got: Vec<u64> = i
            .match_axis(TaxonomyAxis::Hardware, "memory_mb", Some("65536"))
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        got.sort();
        assert_eq!(got, vec![0x1111]);
    }

    /// `filter` with a 2-leaf AND predicate — `exists hardware.gpu`
    /// AND `metadata.intent == "ml-training"` finds the two
    /// GPU+ml-training nodes.
    #[test]
    fn filter_with_composite_predicate() {
        let i = idx();
        let pred = Predicate::and(vec![
            Predicate::exists(TagKey::new(TaxonomyAxis::Hardware, "gpu".to_string())),
            Predicate::metadata_equals("intent", "ml-training"),
        ]);
        let mut got: Vec<u64> = i.filter(&pred).into_iter().map(|(n, _)| n).collect();
        got.sort();
        assert_eq!(got, vec![0x1111, 0x3333]);
    }

    /// `aggregate` with `Count` over the same predicate returns
    /// the cardinality.
    #[test]
    fn aggregate_count_matches_filter_len() {
        let i = idx();
        let pred = Predicate::exists(TagKey::new(
            TaxonomyAxis::Hardware,
            "gpu".to_string(),
        ));
        let count = i.aggregate(&pred, Count::default());
        assert_eq!(count, 2);
    }

    /// `aggregate` with `UniqueAxisValues` collects distinct
    /// memory_mb values across the GPU-tagged nodes.
    #[test]
    fn aggregate_unique_axis_values_collects_distinct() {
        let i = idx();
        let pred = Predicate::exists(TagKey::new(
            TaxonomyAxis::Hardware,
            "gpu".to_string(),
        ));
        let agg = UniqueAxisValues::new(TaxonomyAxis::Hardware, "memory_mb");
        let mut got = i.aggregate(&pred, agg);
        got.sort();
        assert_eq!(got, vec!["16384".to_string(), "65536".to_string()]);
    }

    /// `aggregate` with `MaxNumericMetadata` over a numeric
    /// metadata key returns the maximum across matches.
    #[test]
    fn aggregate_max_numeric_metadata() {
        let i = Arc::new(CapabilityIndex::new());
        let eid = EntityId::from_bytes([0u8; 32]);
        for (id, weight) in [(0xa, "0.3"), (0xb, "0.9"), (0xc, "0.5")] {
            let caps = CapabilitySet::default()
                .add_tag("hardware.gpu")
                .with_metadata("priority_weight", weight);
            i.index(CapabilityAnnouncement::new(id, eid.clone(), 1, caps));
        }
        let pred = Predicate::exists(TagKey::new(
            TaxonomyAxis::Hardware,
            "gpu".to_string(),
        ));
        let agg = MaxNumericMetadata::new("priority_weight");
        let max = i.aggregate(&pred, agg);
        assert_eq!(max, Some(0.9));
    }

    /// Filter that matches nothing returns empty + Count = 0.
    #[test]
    fn empty_match_returns_empty() {
        let i = idx();
        let pred = Predicate::exists(TagKey::new(
            TaxonomyAxis::Hardware,
            "nonexistent_key".to_string(),
        ));
        assert!(i.filter(&pred).is_empty());
        assert_eq!(i.aggregate(&pred, Count::default()), 0);
    }

    /// Composability — chain `match_axis` then a tag-membership
    /// filter on the result. Demonstrates the plan's "no new
    /// operators needed for cross-axis composition" claim:
    /// users compose primitives in their host language.
    #[test]
    fn composes_match_axis_with_post_filter() {
        let i = idx();
        let gpu_nodes = i.match_axis(TaxonomyAxis::Hardware, "gpu", None);
        let us_east_gpu: Vec<u64> = gpu_nodes
            .into_iter()
            .filter(|(_, caps)| {
                caps.metadata.get("region").map(|r| r == "us-east").unwrap_or(false)
            })
            .map(|(n, _)| n)
            .collect();
        assert_eq!(us_east_gpu, vec![0x1111]);
    }

    // =====================================================================
    // Phase E slice 2 tests — traverse + nearest.
    // =====================================================================

    /// Manually add a `Tag::Reserved` to a CapabilitySet via the
    /// privileged `Tag::parse` path (bypasses the `add_tag` /
    /// `parse_user` reserved-prefix rejection).
    fn add_reserved_tag(caps: CapabilitySet, raw: &str) -> CapabilitySet {
        let mut caps = caps;
        let parsed = Tag::parse(raw).unwrap();
        caps.tags.insert(parsed);
        caps
    }

    /// `traverse` walks `fork-of:` parent links up to `max_depth`.
    ///
    /// Setup: three-generation fork chain.
    ///   - root chain `R` lives on node 0xA (carries `causal:R`)
    ///   - fork `F1` of R lives on node 0xB (carries `causal:F1`
    ///     + `fork-of:R`)
    ///   - fork `F2` of F1 lives on node 0xC (carries `causal:F2`
    ///     + `fork-of:F1`)
    ///
    /// Starting from `fork-of:F1` on 0xC, `traverse` should walk
    /// to 0xB (parent F1 host) then 0xA (root R host).
    #[test]
    fn traverse_fork_of_walks_chain() {
        let i = Arc::new(CapabilityIndex::new());
        let eid = EntityId::from_bytes([0u8; 32]);
        // root R on 0xA
        let r_caps = add_reserved_tag(CapabilitySet::default(), "causal:R");
        i.index(CapabilityAnnouncement::new(0xAu64, eid.clone(), 1, r_caps));
        // F1 on 0xB
        let f1_caps = add_reserved_tag(CapabilitySet::default(), "causal:F1");
        let f1_caps = add_reserved_tag(f1_caps, "fork-of:R");
        i.index(CapabilityAnnouncement::new(0xBu64, eid.clone(), 1, f1_caps));
        // F2 on 0xC
        let f2_caps = add_reserved_tag(CapabilitySet::default(), "causal:F2");
        let f2_caps = add_reserved_tag(f2_caps, "fork-of:F1");
        i.index(CapabilityAnnouncement::new(0xCu64, eid.clone(), 1, f2_caps));

        // Start traversal from F2's `fork-of:F1` on 0xC.
        let start_tag = Tag::parse("fork-of:F1").unwrap();
        let path = i.traverse(0xCu64, &start_tag, EdgeKind::ForkOfParent, 5);

        // Expected: [(0xC, fork-of:F1), (0xB, causal:F1), (0xA, causal:R)]
        assert_eq!(path.len(), 3, "path: {path:?}");
        assert_eq!(path[0].0, 0xC);
        assert_eq!(path[1].0, 0xB);
        assert_eq!(path[2].0, 0xA);
    }

    /// `traverse` honors `max_depth` — capping at 1 means just
    /// the start tag + first parent-host hop.
    #[test]
    fn traverse_honors_max_depth() {
        let i = Arc::new(CapabilityIndex::new());
        let eid = EntityId::from_bytes([0u8; 32]);
        let r_caps = add_reserved_tag(CapabilitySet::default(), "causal:R");
        i.index(CapabilityAnnouncement::new(0xAu64, eid.clone(), 1, r_caps));
        let f1_caps = add_reserved_tag(CapabilitySet::default(), "causal:F1");
        let f1_caps = add_reserved_tag(f1_caps, "fork-of:R");
        i.index(CapabilityAnnouncement::new(0xBu64, eid.clone(), 1, f1_caps));

        let start = Tag::parse("fork-of:R").unwrap();
        let path = i.traverse(0xBu64, &start, EdgeKind::ForkOfParent, 0);
        // max_depth = 0: only start.
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].0, 0xB);

        let path = i.traverse(0xBu64, &start, EdgeKind::ForkOfParent, 1);
        // max_depth = 1: start + parent host.
        assert_eq!(path.len(), 2);
        assert_eq!(path[1].0, 0xA);
    }

    /// `traverse` terminates at root when no further `fork-of:`
    /// tag exists. Root chain has no parent link.
    #[test]
    fn traverse_terminates_at_root() {
        let i = Arc::new(CapabilityIndex::new());
        let eid = EntityId::from_bytes([0u8; 32]);
        let r_caps = add_reserved_tag(CapabilitySet::default(), "causal:R");
        i.index(CapabilityAnnouncement::new(0xAu64, eid.clone(), 1, r_caps));
        let f1_caps = add_reserved_tag(CapabilitySet::default(), "causal:F1");
        let f1_caps = add_reserved_tag(f1_caps, "fork-of:R");
        i.index(CapabilityAnnouncement::new(0xBu64, eid.clone(), 1, f1_caps));

        let start = Tag::parse("fork-of:R").unwrap();
        let path = i.traverse(0xBu64, &start, EdgeKind::ForkOfParent, 100);
        // Root has no fork-of, so walk halts at R's host.
        assert_eq!(path.len(), 2);
        assert_eq!(path[1].0, 0xA);
    }

    /// `traverse` halts when the parent chain isn't hosted by
    /// any indexed node — the index doesn't know who has
    /// `causal:<unknown>`.
    #[test]
    fn traverse_halts_when_parent_chain_unknown() {
        let i = Arc::new(CapabilityIndex::new());
        let eid = EntityId::from_bytes([0u8; 32]);
        let f1_caps = add_reserved_tag(CapabilitySet::default(), "causal:F1");
        let f1_caps = add_reserved_tag(f1_caps, "fork-of:UNKNOWN_PARENT");
        i.index(CapabilityAnnouncement::new(0xBu64, eid.clone(), 1, f1_caps));

        let start = Tag::parse("fork-of:UNKNOWN_PARENT").unwrap();
        let path = i.traverse(0xBu64, &start, EdgeKind::ForkOfParent, 5);
        // Just the start — parent chain has no holder.
        assert_eq!(path.len(), 1);
    }

    /// `nearest` ranks survivors by RTT, ascending. Candidates
    /// with no RTT data sort to the back.
    #[test]
    fn nearest_ranks_by_rtt_ascending() {
        let i = idx();
        let pred = Predicate::exists(TagKey::new(
            TaxonomyAxis::Hardware,
            "memory_mb".to_string(),
        ));
        // 0x1111: 5 ms, 0x2222: 50 ms, 0x3333: 1 ms, 0x4444: 100 ms
        let rtts = |id: u64| -> Option<Duration> {
            match id {
                0x1111 => Some(Duration::from_millis(5)),
                0x2222 => Some(Duration::from_millis(50)),
                0x3333 => Some(Duration::from_millis(1)),
                0x4444 => Some(Duration::from_millis(100)),
                _ => None,
            }
        };
        let top = i.nearest(&pred, rtts, 3);
        let ids: Vec<u64> = top.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(ids, vec![0x3333, 0x1111, 0x2222]);
    }

    /// `nearest` truncates at `n`. Pin the `n=0` case + larger-
    /// than-corpus case.
    #[test]
    fn nearest_truncates_at_n() {
        let i = idx();
        let pred = Predicate::exists(TagKey::new(
            TaxonomyAxis::Hardware,
            "memory_mb".to_string(),
        ));
        let rtts = |_: u64| Some(Duration::from_millis(1));
        assert!(i.nearest(&pred, &rtts, 0).is_empty());
        // Larger-than-corpus n returns all 3 memory_mb-bearing nodes.
        let all = i.nearest(&pred, &rtts, 100);
        assert_eq!(all.len(), 3);
    }

    /// `nearest` candidates with no RTT data sort to the back;
    /// ties broken by lex NodeId.
    #[test]
    fn nearest_unmeasured_candidates_sort_last() {
        let i = idx();
        let pred = Predicate::exists(TagKey::new(
            TaxonomyAxis::Hardware,
            "memory_mb".to_string(),
        ));
        // Only 0x2222 has RTT data.
        let rtts = |id: u64| -> Option<Duration> {
            if id == 0x2222 {
                Some(Duration::from_millis(10))
            } else {
                None
            }
        };
        let ranked = i.nearest(&pred, rtts, 100);
        let ids: Vec<u64> = ranked.iter().map(|(n, _, _)| *n).collect();
        // 0x2222 first (has RTT); rest in lex order.
        assert_eq!(ids[0], 0x2222);
        // Rest sorted lex.
        assert!(ids[1] <= ids[2]);
    }
}
