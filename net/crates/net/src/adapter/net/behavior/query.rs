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
    predicate::Predicate, tag::Tag, tag::TagKey, tag::TaxonomyAxis,
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
    fn observe(&mut self, _: u64, _: &crate::adapter::net::behavior::capability::CapabilitySet) {
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

    fn observe(&mut self, _: u64, caps: &crate::adapter::net::behavior::capability::CapabilitySet) {
        for tag in &caps.tags {
            if let Tag::AxisValue {
                axis, key, value, ..
            } = tag
            {
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

    fn observe(&mut self, _: u64, caps: &crate::adapter::net::behavior::capability::CapabilitySet) {
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
/// Reference impl previously lived on the legacy
/// `CapabilityIndex`; that store was removed in Phase 3B of the
/// multifold migration. Downstream consumers layer their own
/// implementations (federated, in-memory test fixtures, the
/// capability fold) without changing the trait surface.
pub trait CapabilityQuery {
    /// Scan the index, returning `(node_id, caps)` pairs whose
    /// tags + metadata satisfy `predicate`. Ordering is
    /// implementation-defined; callers that need stable order
    /// sort the result.
    fn filter(
        &self,
        predicate: &Predicate,
    ) -> Vec<(
        u64,
        crate::adapter::net::behavior::capability::CapabilitySet,
    )>;

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
    ) -> Vec<(
        u64,
        crate::adapter::net::behavior::capability::CapabilitySet,
    )>;

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
// Helpers
// =========================================================================

/// Walk `caps.tags` looking for `axis.key` (`value=None` →
/// presence-OR-value-form match; `value=Some(s)` → exact
/// `axis.key=s` value-form match). Cheaper than building an
/// EvalContext + running a Predicate AST since we walk the tag
/// set once with a tight `match`.
///
/// Kept (with `#[allow(dead_code)]`) for downstream `CapabilityQuery`
/// implementors — the legacy `CapabilityIndex` impl was removed in
/// Phase 3B of the multifold migration.
#[allow(dead_code)]
fn axis_match(
    caps: &crate::adapter::net::behavior::capability::CapabilitySet,
    axis: TaxonomyAxis,
    key: &str,
    value: Option<&str>,
) -> bool {
    for tag in &caps.tags {
        match tag {
            Tag::AxisPresent {
                axis: tag_axis,
                key: tag_key,
            } if *tag_axis == axis && tag_key == key && value.is_none() => {
                return true;
            }
            Tag::AxisValue {
                axis: tag_axis,
                key: tag_key,
                value: tag_value,
                ..
            } => {
                if *tag_axis != axis || tag_key != key {
                    continue;
                }
                match value {
                    None => return true, // any value satisfies presence
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

