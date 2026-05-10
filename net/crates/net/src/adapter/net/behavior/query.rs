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

use crate::adapter::net::behavior::{
    capability::CapabilityIndex, predicate::EvalContext, predicate::Predicate, tag::Tag,
    tag::TagKey, tag::TaxonomyAxis,
};

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
}
