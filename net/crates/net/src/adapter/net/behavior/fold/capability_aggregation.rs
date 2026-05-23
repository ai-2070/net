//! Capacity-aggregation surface on top of [`Fold<CapabilityFold>`].
//!
//! Composes three orthogonal axes — `TagMatcher × GroupBy ×
//! Aggregation` — into a single materialized-view method,
//! [`Fold::aggregate`](super::Fold::aggregate). Operators ask
//! "what's available, bucketed how, counted how" and the fold answers
//! by walking its live `(class, node) → CapabilityMembership` store
//! once.
//!
//! Sub-step 6c-A scope: ships the matcher / group_by / aggregation
//! variants that don't need regex / semver / numeric-tag parsing.
//! `Regex`, `VersionRange`, `SumNumericTag`, and `Min/MaxNumericTag`
//! land in 6c-B (capacity ranking) and 6c-C (advanced matchers).
//!
//! See `docs/plans/MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::capability::{CapabilityFold, CapabilityMembership, NodeState};
use super::state::NodeId;
use super::Fold;
use crate::adapter::net::behavior::tag::{Tag, TaxonomyAxis};

/// Pre-grouping filter — picks which entries the aggregation walks.
///
/// Applied against each entry's `tags` array; an entry is included if
/// ANY of its tags matches the matcher. The 6c-A scope covers the
/// four variants that don't pull in `regex` or `semver` dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagMatcher {
    /// Exact tag string match — e.g. `"software.python=3.11"` matches
    /// only entries carrying that exact canonical tag.
    Exact(String),
    /// Tag-string prefix — e.g. `"hardware.gpu"` matches
    /// `"hardware.gpu"` and `"hardware.gpu.vram_gb=80"` and any other
    /// tag starting with the prefix.
    Prefix(String),
    /// Tag is anywhere in the given taxonomy axis. Matches every
    /// axis-prefixed tag (presence + value) in that axis.
    Axis(TaxonomyAxis),
    /// Tag has a specific (axis, key) regardless of value.
    /// `AxisKey { axis: Hardware, key: "gpu.count" }` matches
    /// `"hardware.gpu.count=8"` and `"hardware.gpu.count=16"` but not
    /// `"hardware.gpu.vram_gb=80"`.
    AxisKey {
        /// Taxonomy axis the tag must live in.
        axis: TaxonomyAxis,
        /// Key portion after the `<axis>.` prefix, regardless of any
        /// value the tag may carry.
        key: String,
    },
}

impl TagMatcher {
    /// `true` if any tag in `tags` matches this matcher.
    fn matches_any(&self, tags: &[String]) -> bool {
        tags.iter().any(|t| self.matches_one(t))
    }

    fn matches_one(&self, raw: &str) -> bool {
        match self {
            Self::Exact(s) => raw == s,
            Self::Prefix(s) => raw.starts_with(s),
            Self::Axis(want) => Tag::parse(raw)
                .ok()
                .and_then(|t| t.axis_key().map(|k| k.axis))
                == Some(*want),
            Self::AxisKey { axis, key } => Tag::parse(raw)
                .ok()
                .and_then(|t| t.axis_key())
                .is_some_and(|k| k.axis == *axis && k.key == *key),
        }
    }
}

/// Bucket-key derivation — for each matching entry, decides which
/// bucket(s) it contributes to. Most variants produce one bucket per
/// entry; `TagStem` and `TagValue` can produce zero, one, or many
/// (one per matching tag on the entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupBy {
    /// Each entry's `class_hash`, rendered as `"0x{:x}"`.
    Class,
    /// Each entry's `state` (`"idle"` / `"busy"` / `"reserved"` /
    /// `"faulty"`).
    State,
    /// Each entry's `region` (or `"(none)"` for unset).
    Region,
    /// Each entry's publisher `node_id`, rendered as `"0x{:x}"`.
    Publisher,
    /// Bucket by tag stem. For each tag matching `<prefix>` or
    /// `<prefix>.<rest>`, the bucket key is the next dotted segment
    /// after the prefix. `TagStem("hardware.gpu")` over a tag set
    /// containing `"hardware.gpu.h100"` and `"hardware.gpu.a100"`
    /// produces buckets `"h100"` and `"a100"`. Bare `"hardware.gpu"`
    /// itself produces the bucket `"(present)"` so presence-only tags
    /// don't disappear.
    TagStem(String),
    /// Bucket by the value of a specific axis-key. For each
    /// `AxisValue { axis, key, value }` tag on the entry matching the
    /// requested `(axis, key)`, the bucket key is the captured value.
    TagValue {
        /// Taxonomy axis the tag must live in.
        axis: TaxonomyAxis,
        /// Key portion the tag must carry; bucket key is the value
        /// portion after the separator.
        key: String,
    },
}

impl GroupBy {
    /// Compute the bucket keys this entry contributes to. Returns a
    /// `Vec` since `TagStem` / `TagValue` may produce multiple
    /// buckets per entry.
    fn bucket_keys(
        &self,
        membership: &CapabilityMembership,
        publisher: NodeId,
    ) -> Vec<String> {
        match self {
            Self::Class => vec![format!("0x{:x}", membership.class_hash)],
            Self::State => vec![state_label(membership.state).to_string()],
            Self::Region => vec![
                membership
                    .region
                    .clone()
                    .unwrap_or_else(|| "(none)".to_string()),
            ],
            Self::Publisher => vec![format!("0x{:x}", publisher)],
            Self::TagStem(prefix) => {
                let mut buckets: Vec<String> = membership
                    .tags
                    .iter()
                    .filter_map(|t| tag_stem_after(t, prefix))
                    .collect();
                buckets.sort();
                buckets.dedup();
                buckets
            }
            Self::TagValue { axis, key } => {
                let mut values: Vec<String> = membership
                    .tags
                    .iter()
                    .filter_map(|raw| axis_value_for(raw, *axis, key))
                    .collect();
                values.sort();
                values.dedup();
                values
            }
        }
    }
}

/// Per-bucket reduction — once entries are bucketed, this decides
/// what numeric value lands in the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Aggregation {
    /// Number of entries in each bucket. The natural "how many".
    Count,
    /// Distinct publisher `node_id`s contributing to the bucket. An
    /// entry contributing under multiple buckets counts once per
    /// bucket but doesn't double-count its publisher in any given
    /// bucket.
    DistinctPublishers,
    /// Distinct values for the given `(axis, key)` observed across
    /// entries in the bucket. `DistinctValues { axis: Hardware, key:
    /// "gpu.vram_gb" }` answers "how many distinct GPU memory sizes
    /// are running in this region?"
    DistinctValues {
        /// Taxonomy axis the value-bearing tag lives in.
        axis: TaxonomyAxis,
        /// Key portion of the value-bearing tag.
        key: String,
    },
}

impl Fold<CapabilityFold> {
    /// Walk the fold once and produce a `Vec<(bucket, value)>` sorted
    /// lexicographically by bucket key.
    ///
    /// `matcher = None` includes every entry; otherwise an entry is
    /// included only if at least one of its tags matches the matcher.
    /// `group_by` decides how matching entries are bucketed (one
    /// entry can land in multiple buckets via `TagStem` / `TagValue`).
    /// `agg` decides what numeric value each bucket carries.
    ///
    /// Returns an empty `Vec` when no entries match. Bucket order is
    /// stable across calls so operator tooling can diff snapshots.
    pub fn aggregate(
        &self,
        matcher: Option<TagMatcher>,
        group_by: GroupBy,
        agg: Aggregation,
    ) -> Vec<(String, u64)> {
        // Phase 1: walk state once, materialize a per-bucket
        // accumulator. We need to track publishers and observed values
        // separately because the aggregation type decides which to
        // count.
        let mut buckets: HashMap<String, BucketAccum> = HashMap::new();

        self.with_state(|state| {
            for ((_class, publisher), entry) in state.entries.iter() {
                let membership = &entry.payload;
                if let Some(m) = &matcher {
                    if !m.matches_any(&membership.tags) {
                        continue;
                    }
                }
                let keys = group_by.bucket_keys(membership, *publisher);
                if keys.is_empty() {
                    continue;
                }
                for key in keys {
                    let slot = buckets.entry(key).or_default();
                    slot.count = slot.count.saturating_add(1);
                    slot.publishers.insert(*publisher);
                    if let Aggregation::DistinctValues { axis, key: k } = &agg {
                        for raw in &membership.tags {
                            if let Some(v) = axis_value_for(raw, *axis, k) {
                                slot.distinct_values.insert(v);
                            }
                        }
                    }
                }
            }
        });

        // Phase 2: project to the requested aggregation, sort by
        // bucket key.
        let mut rows: Vec<(String, u64)> = buckets
            .into_iter()
            .map(|(bucket, slot)| {
                let v: u64 = match &agg {
                    Aggregation::Count => slot.count,
                    Aggregation::DistinctPublishers => slot.publishers.len() as u64,
                    Aggregation::DistinctValues { .. } => slot.distinct_values.len() as u64,
                };
                (bucket, v)
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }
}

#[derive(Default)]
struct BucketAccum {
    /// Raw entry count contributing to this bucket. One entry that
    /// contributes via two `TagStem` buckets counts once in each
    /// bucket's `count` (which matches what an operator means by
    /// "how many entries in this bucket").
    count: u64,
    /// Publisher `node_id`s contributing to this bucket. Set
    /// semantics — two entries from the same publisher count as one
    /// in `DistinctPublishers`.
    publishers: HashSet<NodeId>,
    /// Observed `(axis, key) → value` strings for
    /// `Aggregation::DistinctValues`.
    distinct_values: HashSet<String>,
}

/// Canonical lowercase state name. Same shape as the wire form
/// `serde(rename_all = "snake_case")` produces.
fn state_label(state: NodeState) -> &'static str {
    match state {
        NodeState::Idle => "idle",
        NodeState::Busy => "busy",
        NodeState::Reserved => "reserved",
        NodeState::Faulty => "faulty",
    }
}

/// Strip `<prefix>` off `tag` and return the next dotted segment
/// (everything up to the next `.`, `=`, or `:`).
/// - `"hardware.gpu.h100"` with prefix `"hardware.gpu"` → `"h100"`.
/// - `"hardware.gpu"` with prefix `"hardware.gpu"` → `"(present)"`.
/// - `"hardware.gpu.vram_gb=80"` with prefix `"hardware.gpu"` →
///   `"vram_gb"`.
/// - non-matching tag → `None`.
fn tag_stem_after(tag: &str, prefix: &str) -> Option<String> {
    let rest = tag.strip_prefix(prefix)?;
    if rest.is_empty() {
        // Exact match on prefix; presence form gets its own bucket so
        // it doesn't silently merge with a missing-stem case.
        return Some("(present)".to_string());
    }
    let rest = rest.strip_prefix('.')?;
    let stem_end = rest.find(['.', '=', ':']).unwrap_or(rest.len());
    if stem_end == 0 {
        None
    } else {
        Some(rest[..stem_end].to_string())
    }
}

/// Extract the value of an `AxisValue` tag matching `(axis, key)`.
/// Returns `None` for `AxisPresent`, `Reserved`, or `Legacy` tags, or
/// when the axis-key pair doesn't match.
fn axis_value_for(raw: &str, want_axis: TaxonomyAxis, want_key: &str) -> Option<String> {
    let tag = Tag::parse(raw).ok()?;
    match tag {
        Tag::AxisValue {
            axis, key, value, ..
        } if axis == want_axis && key == want_key => Some(value),
        _ => None,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::fold::wire::SignedAnnouncement;
    use crate::adapter::net::behavior::fold::EnvelopeMeta;
    use crate::adapter::net::behavior::fold::FoldKind;
    use crate::adapter::net::identity::EntityKeypair;
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn new_fold() -> Fold<CapabilityFold> {
        Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO)
    }

    fn sign(
        kp: &EntityKeypair,
        publisher: NodeId,
        class: u64,
        tags: &[&str],
        state: NodeState,
        region: Option<&str>,
    ) -> SignedAnnouncement<CapabilityMembership> {
        SignedAnnouncement::sign(
            kp,
            CapabilityFold::KIND_ID,
            class,
            publisher,
            1,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: class,
                tags: tags.iter().map(|s| (*s).to_string()).collect(),
                hardware: None,
                state,
                region: region.map(|s| s.to_string()),
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
            },
        )
        .expect("sign")
    }

    fn populated_fold() -> Fold<CapabilityFold> {
        // Three publishers, mix of GPU types + regions + states.
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        // 0xA — h100 / us-east / idle
        fold.apply(sign(
            &kp,
            0xA,
            0x100,
            &[
                "hardware.gpu",
                "hardware.gpu.h100",
                "hardware.gpu.count=8",
                "software.python=3.11",
            ],
            NodeState::Idle,
            Some("us-east"),
        ))
        .unwrap();
        // 0xB — h100 / us-east / busy
        fold.apply(sign(
            &kp,
            0xB,
            0x100,
            &[
                "hardware.gpu",
                "hardware.gpu.h100",
                "hardware.gpu.count=4",
                "software.python=3.12",
            ],
            NodeState::Busy,
            Some("us-east"),
        ))
        .unwrap();
        // 0xC — a100 / us-west / idle
        fold.apply(sign(
            &kp,
            0xC,
            0x200,
            &[
                "hardware.gpu",
                "hardware.gpu.a100",
                "hardware.gpu.count=2",
                "software.python=3.11",
            ],
            NodeState::Idle,
            Some("us-west"),
        ))
        .unwrap();
        fold
    }

    // ── TagMatcher variants ────────────────────────────────────

    #[test]
    fn matcher_exact_picks_only_exact_tag() {
        let fold = populated_fold();
        let rows = fold.aggregate(
            Some(TagMatcher::Exact("software.python=3.11".into())),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        let publishers: Vec<&str> = rows.iter().map(|(b, _)| b.as_str()).collect();
        assert_eq!(publishers, vec!["0xa", "0xc"]);
    }

    #[test]
    fn matcher_prefix_picks_everything_under_the_prefix() {
        let fold = populated_fold();
        // Every entry has at least one `hardware.gpu*` tag → all three
        // publishers match.
        let rows = fold.aggregate(
            Some(TagMatcher::Prefix("hardware.gpu".into())),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn matcher_axis_picks_every_entry_in_that_axis() {
        let fold = populated_fold();
        let rows = fold.aggregate(
            Some(TagMatcher::Axis(TaxonomyAxis::Hardware)),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert_eq!(rows.len(), 3, "every entry has a hardware.* tag");
    }

    #[test]
    fn matcher_axis_key_picks_only_entries_with_that_key() {
        let fold = populated_fold();
        // (Hardware, "gpu.count") matches every entry — all three have
        // `hardware.gpu.count=N`.
        let rows = fold.aggregate(
            Some(TagMatcher::AxisKey {
                axis: TaxonomyAxis::Hardware,
                key: "gpu.count".into(),
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert_eq!(rows.len(), 3);

        // (Software, "python") matches every entry too — `python=3.11`
        // / `python=3.12` are `AxisValue { key: "python", ... }`.
        let rows = fold.aggregate(
            Some(TagMatcher::AxisKey {
                axis: TaxonomyAxis::Software,
                key: "python".into(),
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert_eq!(rows.len(), 3);

        // (Hardware, "nonexistent") matches none.
        let rows = fold.aggregate(
            Some(TagMatcher::AxisKey {
                axis: TaxonomyAxis::Hardware,
                key: "nonexistent".into(),
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn no_matcher_includes_every_entry() {
        let fold = populated_fold();
        let rows = fold.aggregate(None, GroupBy::Publisher, Aggregation::Count);
        assert_eq!(rows.len(), 3);
    }

    // ── GroupBy variants ────────────────────────────────────────

    #[test]
    fn group_by_class_buckets_by_class_hash() {
        let fold = populated_fold();
        let rows = fold.aggregate(None, GroupBy::Class, Aggregation::Count);
        assert_eq!(
            rows,
            vec![("0x100".to_string(), 2), ("0x200".to_string(), 1)]
        );
    }

    #[test]
    fn group_by_state_buckets_idle_busy_reserved_faulty() {
        let fold = populated_fold();
        let rows = fold.aggregate(None, GroupBy::State, Aggregation::Count);
        assert_eq!(
            rows,
            vec![("busy".to_string(), 1), ("idle".to_string(), 2)]
        );
    }

    #[test]
    fn group_by_region_renders_none_as_explicit_string() {
        let fold = populated_fold();
        let rows = fold.aggregate(None, GroupBy::Region, Aggregation::Count);
        assert_eq!(
            rows,
            vec![("us-east".to_string(), 2), ("us-west".to_string(), 1)]
        );

        // Now add a region-less publisher.
        let kp = EntityKeypair::generate();
        fold.apply(sign(&kp, 0xD, 0x300, &[], NodeState::Idle, None))
            .unwrap();
        let rows = fold.aggregate(None, GroupBy::Region, Aggregation::Count);
        assert_eq!(
            rows,
            vec![
                ("(none)".to_string(), 1),
                ("us-east".to_string(), 2),
                ("us-west".to_string(), 1),
            ]
        );
    }

    #[test]
    fn group_by_publisher_buckets_by_node_id_hex() {
        let fold = populated_fold();
        let rows = fold.aggregate(None, GroupBy::Publisher, Aggregation::Count);
        assert_eq!(
            rows,
            vec![
                ("0xa".to_string(), 1),
                ("0xb".to_string(), 1),
                ("0xc".to_string(), 1),
            ]
        );
    }

    #[test]
    fn group_by_tag_stem_buckets_per_dotted_stem_after_prefix() {
        let fold = populated_fold();
        // `hardware.gpu` stems: h100 (2 publishers), a100 (1),
        // count (3 — every entry has a `hardware.gpu.count=N`),
        // plus the bare `hardware.gpu` becomes "(present)" for each.
        let rows = fold.aggregate(
            None,
            GroupBy::TagStem("hardware.gpu".into()),
            Aggregation::Count,
        );
        let map: HashMap<String, u64> = rows.into_iter().collect();
        assert_eq!(map.get("h100").copied(), Some(2));
        assert_eq!(map.get("a100").copied(), Some(1));
        assert_eq!(map.get("count").copied(), Some(3));
        assert_eq!(map.get("(present)").copied(), Some(3));
    }

    #[test]
    fn group_by_tag_value_extracts_value_after_separator() {
        let fold = populated_fold();
        let rows = fold.aggregate(
            None,
            GroupBy::TagValue {
                axis: TaxonomyAxis::Software,
                key: "python".into(),
            },
            Aggregation::Count,
        );
        assert_eq!(
            rows,
            vec![("3.11".to_string(), 2), ("3.12".to_string(), 1)]
        );
    }

    // ── Aggregation variants ───────────────────────────────────

    #[test]
    fn aggregation_count_returns_entry_count_per_bucket() {
        let fold = populated_fold();
        let rows = fold.aggregate(None, GroupBy::Region, Aggregation::Count);
        assert_eq!(
            rows,
            vec![("us-east".to_string(), 2), ("us-west".to_string(), 1)]
        );
    }

    #[test]
    fn aggregation_distinct_publishers_dedupes_per_bucket() {
        // Two entries from the SAME publisher in two classes; bucket
        // by region. `DistinctPublishers` should report 1 publisher
        // in that region, not 2.
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign(&kp, 0xA, 0x100, &[], NodeState::Idle, Some("us-east")))
            .unwrap();
        fold.apply(sign(&kp, 0xA, 0x200, &[], NodeState::Idle, Some("us-east")))
            .unwrap();
        fold.apply(sign(&kp, 0xB, 0x100, &[], NodeState::Idle, Some("us-east")))
            .unwrap();

        let by_count = fold.aggregate(None, GroupBy::Region, Aggregation::Count);
        assert_eq!(by_count, vec![("us-east".to_string(), 3)]);

        let by_publishers =
            fold.aggregate(None, GroupBy::Region, Aggregation::DistinctPublishers);
        assert_eq!(by_publishers, vec![("us-east".to_string(), 2)]);
    }

    #[test]
    fn aggregation_distinct_values_counts_unique_values_per_bucket() {
        let fold = populated_fold();
        // For each region, count distinct python versions.
        let rows = fold.aggregate(
            None,
            GroupBy::Region,
            Aggregation::DistinctValues {
                axis: TaxonomyAxis::Software,
                key: "python".into(),
            },
        );
        // us-east has 3.11 (0xA) + 3.12 (0xB) → 2 distinct.
        // us-west has 3.11 (0xC) → 1 distinct.
        assert_eq!(
            rows,
            vec![("us-east".to_string(), 2), ("us-west".to_string(), 1)]
        );
    }

    // ── Composition ─────────────────────────────────────────────

    #[test]
    fn matcher_narrows_before_grouping() {
        let fold = populated_fold();
        // Only h100 publishers, bucketed by region. 0xA + 0xB are both
        // h100 / us-east; 0xC is a100 / us-west and is filtered out.
        let rows = fold.aggregate(
            Some(TagMatcher::Exact("hardware.gpu.h100".into())),
            GroupBy::Region,
            Aggregation::Count,
        );
        assert_eq!(rows, vec![("us-east".to_string(), 2)]);
    }

    #[test]
    fn empty_fold_aggregates_to_empty_vec() {
        let fold = new_fold();
        let rows = fold.aggregate(None, GroupBy::Region, Aggregation::Count);
        assert!(rows.is_empty());
    }

    #[test]
    fn matcher_that_excludes_everything_returns_empty() {
        let fold = populated_fold();
        let rows = fold.aggregate(
            Some(TagMatcher::Exact("nope".into())),
            GroupBy::Region,
            Aggregation::Count,
        );
        assert!(rows.is_empty());
    }

    // ── Helpers ─────────────────────────────────────────────────

    #[test]
    fn tag_stem_after_handles_bare_presence_form() {
        assert_eq!(
            tag_stem_after("hardware.gpu", "hardware.gpu"),
            Some("(present)".to_string())
        );
    }

    #[test]
    fn tag_stem_after_extracts_segment_up_to_next_separator() {
        assert_eq!(
            tag_stem_after("hardware.gpu.h100", "hardware.gpu"),
            Some("h100".to_string())
        );
        assert_eq!(
            tag_stem_after("hardware.gpu.vram_gb=80", "hardware.gpu"),
            Some("vram_gb".to_string())
        );
        assert_eq!(
            tag_stem_after("hardware.gpu.count:8", "hardware.gpu"),
            Some("count".to_string())
        );
    }

    #[test]
    fn tag_stem_after_returns_none_for_non_matching_tag() {
        assert_eq!(tag_stem_after("software.python=3.11", "hardware.gpu"), None);
    }
}
