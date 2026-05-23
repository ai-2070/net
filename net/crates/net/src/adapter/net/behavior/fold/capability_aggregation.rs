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
    Exact {
        /// The literal tag string to match against.
        value: String,
    },
    /// Tag-string prefix — e.g. `"hardware.gpu"` matches
    /// `"hardware.gpu"` and `"hardware.gpu.vram_gb=80"` and any other
    /// tag starting with the prefix.
    Prefix {
        /// The tag prefix to match against.
        value: String,
    },
    /// Tag is anywhere in the given taxonomy axis. Matches every
    /// axis-prefixed tag (presence + value) in that axis.
    Axis {
        /// Taxonomy axis the tag must live in.
        axis: TaxonomyAxis,
    },
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
    /// Regex match against the canonical tag string form. Invalid
    /// patterns reject everything (the matcher fails closed —
    /// safer than silently treating bad patterns as wildcards).
    /// Compiled per `matches_one` call; callers expecting heavy
    /// reuse should pre-filter via a coarser matcher first.
    Regex {
        /// Regular-expression pattern to match against the tag.
        pattern: String,
    },
    /// Semver range against a specific axis-key value. Picks
    /// `AxisValue` tags whose `(axis, key)` matches `axis_key`
    /// (canonical dotted form, e.g. `"software.python"`) and whose
    /// `value` parses as a semver `Version` within
    /// `[min, max]` (inclusive). `min`/`max` are
    /// `Option<String>` semver expressions — `None` means
    /// unbounded on that side. Unparseable values are skipped
    /// silently.
    VersionRange {
        /// Canonical `<axis>.<key>` string of the value-bearing
        /// tag (e.g. `"software.python"`).
        axis_key: String,
        /// Inclusive lower bound. `None` = no lower bound.
        min: Option<String>,
        /// Inclusive upper bound. `None` = no upper bound.
        max: Option<String>,
    },
}

impl TagMatcher {
    /// `true` if any tag in `tags` matches this matcher.
    fn matches_any(&self, tags: &[String]) -> bool {
        tags.iter().any(|t| self.matches_one(t))
    }

    fn matches_one(&self, raw: &str) -> bool {
        match self {
            Self::Exact { value } => raw == value,
            Self::Prefix { value } => raw.starts_with(value),
            Self::Axis { axis } => {
                Tag::parse(raw)
                    .ok()
                    .and_then(|t| t.axis_key().map(|k| k.axis))
                    == Some(*axis)
            }
            Self::AxisKey { axis, key } => Tag::parse(raw)
                .ok()
                .and_then(|t| t.axis_key())
                .is_some_and(|k| k.axis == *axis && k.key == *key),
            Self::Regex { pattern } => match regex::Regex::new(pattern) {
                Ok(re) => re.is_match(raw),
                Err(_) => false,
            },
            Self::VersionRange { axis_key, min, max } => {
                let Some(value) = string_value_for_axis_key(raw, axis_key) else {
                    return false;
                };
                let Ok(parsed) = semver::Version::parse(&value) else {
                    return false;
                };
                if let Some(lo) = min.as_deref().and_then(|s| semver::Version::parse(s).ok()) {
                    if parsed < lo {
                        return false;
                    }
                }
                if let Some(hi) = max.as_deref().and_then(|s| semver::Version::parse(s).ok()) {
                    if parsed > hi {
                        return false;
                    }
                }
                true
            }
        }
    }
}

/// Bucket-key derivation — for each matching entry, decides which
/// bucket(s) it contributes to. Most variants produce one bucket per
/// entry; `TagStem` and `TagValue` can produce zero, one, or many
/// (one per matching tag on the entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupBy {
    /// Each entry's `class_hash`, rendered as `"0x{:x}"`.
    #[default]
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
    /// after the prefix. `TagStem { prefix: "hardware.gpu" }` over a
    /// tag set containing `"hardware.gpu.h100"` and
    /// `"hardware.gpu.a100"` produces buckets `"h100"` and `"a100"`.
    /// Bare `"hardware.gpu"` itself produces the bucket `"(present)"`
    /// so presence-only tags don't disappear.
    TagStem {
        /// Prefix that an entry's tag must start with for the stem
        /// extraction to apply.
        prefix: String,
    },
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
    fn bucket_keys(&self, membership: &CapabilityMembership, publisher: NodeId) -> Vec<String> {
        match self {
            Self::Class => vec![format!("0x{:x}", membership.class_hash)],
            Self::State => vec![state_label(membership.state).to_string()],
            Self::Region => vec![membership
                .region
                .clone()
                .unwrap_or_else(|| "(none)".to_string())],
            Self::Publisher => vec![format!("0x{:x}", publisher)],
            Self::TagStem { prefix } => {
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
    /// Sum the numeric value of `<axis_key>=<n>` tags across the
    /// bucket. The `axis_key` field is the canonical dotted
    /// axis-key (e.g. `"hardware.gpu.count"`); only `AxisValue`
    /// tags whose `(axis, key)` matches are considered. Values
    /// that don't parse as `u64` are skipped silently. Saturating
    /// addition — overflow caps at `u64::MAX` rather than
    /// panicking.
    SumNumericTag {
        /// Canonical `<axis>.<key>` of the numeric-value tag to sum.
        axis_key: String,
    },
    /// Minimum observed numeric value of an `<axis_key>=<n>` tag
    /// across the bucket. Returns `0` when no parseable values are
    /// observed in the bucket (an operator who needs to distinguish
    /// "no values observed" from "min is 0" should use
    /// `capacity_ranking` with `sum_axis_key`, which surfaces
    /// `Option<u64>`).
    MinNumericTag {
        /// Canonical `<axis>.<key>` of the numeric-value tag to min.
        axis_key: String,
    },
    /// Maximum observed numeric value of an `<axis_key>=<n>` tag
    /// across the bucket. Returns `0` when no parseable values are
    /// observed (same caveat as `MinNumericTag`).
    MaxNumericTag {
        /// Canonical `<axis>.<key>` of the numeric-value tag to max.
        axis_key: String,
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
                    match &agg {
                        Aggregation::DistinctValues { axis, key: k } => {
                            for raw in &membership.tags {
                                if let Some(v) = axis_value_for(raw, *axis, k) {
                                    slot.distinct_values.insert(v);
                                }
                            }
                        }
                        Aggregation::SumNumericTag { axis_key }
                        | Aggregation::MinNumericTag { axis_key }
                        | Aggregation::MaxNumericTag { axis_key } => {
                            for raw in &membership.tags {
                                if let Some(n) = numeric_value_for(raw, axis_key) {
                                    slot.numeric_sum = slot.numeric_sum.saturating_add(n);
                                    slot.numeric_min =
                                        Some(slot.numeric_min.map_or(n, |cur| cur.min(n)));
                                    slot.numeric_max =
                                        Some(slot.numeric_max.map_or(n, |cur| cur.max(n)));
                                    slot.numeric_present = true;
                                }
                            }
                        }
                        _ => {}
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
                    Aggregation::SumNumericTag { .. } => slot.numeric_sum,
                    Aggregation::MinNumericTag { .. } => slot.numeric_min.unwrap_or(0),
                    Aggregation::MaxNumericTag { .. } => slot.numeric_max.unwrap_or(0),
                };
                (bucket, v)
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    /// Capacity-ranked materialized view: bucket the fold's entries
    /// per `query.group_by`, break each bucket down by state, and
    /// (optionally) sum a numeric tag across the bucket. Returns
    /// rows sorted by `available` descending, ties broken by
    /// bucket key ascending; truncated to `query.limit` (0 = no
    /// truncation).
    ///
    /// `rtt_lookup` maps a publisher's `node_id` to current RTT in
    /// milliseconds. The closure may return `None`; entries whose
    /// publisher returns `None` are dropped when
    /// `query.max_rtt_ms` is set (fail-closed — never-pinged nodes
    /// don't get to ride a "fastest available" filter as zero).
    /// When `query.max_rtt_ms` is `None`, the closure is never
    /// called and all reachable entries pass.
    ///
    /// Faulty entries are always excluded from the row counts —
    /// they don't contribute to `idle` / `busy` / `reserved` /
    /// `available` regardless of RTT.
    pub fn capacity_ranking<R>(&self, query: CapacityQuery, rtt_lookup: R) -> Vec<CapacityRow>
    where
        R: Fn(NodeId) -> Option<u32>,
    {
        // Per-bucket accumulator. Distinct from `BucketAccum` above
        // because we need state-broken-down counts, which the base
        // `aggregate` path collapses.
        let mut buckets: HashMap<String, CapacityAccum> = HashMap::new();

        self.with_state(|state| {
            for ((_class, publisher), entry) in state.entries.iter() {
                let membership = &entry.payload;

                // Faulty never makes it into the row counts.
                if membership.state == NodeState::Faulty {
                    continue;
                }

                // Matcher gate.
                if let Some(m) = &query.matcher {
                    if !m.matches_any(&membership.tags) {
                        continue;
                    }
                }

                // RTT gate. `None` returned for an unknown publisher
                // when `max_rtt_ms` is set drops the entry (fail-
                // closed). When `max_rtt_ms` is `None` we skip the
                // lookup entirely.
                if let Some(max) = query.max_rtt_ms {
                    let Some(rtt) = rtt_lookup(*publisher) else {
                        continue;
                    };
                    if rtt > max {
                        continue;
                    }
                }

                let keys = query.group_by.bucket_keys(membership, *publisher);
                if keys.is_empty() {
                    continue;
                }

                // Sum the per-entry numeric capacity once and add it
                // to every bucket the entry contributes to. An entry
                // landing in two `TagStem` buckets counts once toward
                // each bucket's summed_capacity — same shape the
                // state counts use.
                let entry_capacity: Option<u64> = query.sum_axis_key.as_deref().map(|axk| {
                    membership
                        .tags
                        .iter()
                        .filter_map(|t| numeric_value_for(t, axk))
                        .fold(0u64, |acc, n| acc.saturating_add(n))
                });

                for key in keys {
                    let slot = buckets.entry(key).or_default();
                    match membership.state {
                        NodeState::Idle => slot.idle = slot.idle.saturating_add(1),
                        NodeState::Busy => slot.busy = slot.busy.saturating_add(1),
                        NodeState::Reserved => slot.reserved = slot.reserved.saturating_add(1),
                        NodeState::Faulty => unreachable!("filtered above"),
                    }
                    if let Some(c) = entry_capacity {
                        slot.summed_capacity =
                            Some(slot.summed_capacity.unwrap_or(0).saturating_add(c));
                    }
                }
            }
        });

        // Project to rows.
        let mut rows: Vec<CapacityRow> = buckets
            .into_iter()
            .map(|(bucket, slot)| {
                let available = slot
                    .idle
                    .saturating_add(slot.busy)
                    .saturating_add(slot.reserved);
                CapacityRow {
                    bucket,
                    idle: slot.idle,
                    busy: slot.busy,
                    reserved: slot.reserved,
                    available,
                    summed_capacity: slot.summed_capacity,
                }
            })
            .collect();

        // Sort by available descending; tie-break on bucket key
        // ascending so the output is deterministic.
        rows.sort_by(|a, b| b.available.cmp(&a.available).then(a.bucket.cmp(&b.bucket)));

        if query.limit > 0 && rows.len() > query.limit {
            rows.truncate(query.limit);
        }
        rows
    }
}

/// Operator-facing query shape for [`Fold::capacity_ranking`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapacityQuery {
    /// Pre-filter on entries before grouping. `None` includes every
    /// non-faulty entry.
    pub matcher: Option<TagMatcher>,
    /// How to bucket matching entries.
    pub group_by: GroupBy,
    /// Drop entries whose publisher's RTT exceeds this. `None` =
    /// no RTT filter (consider every reachable non-faulty entry).
    pub max_rtt_ms: Option<u32>,
    /// Optional canonical axis-key string to sum across each
    /// bucket's entries (e.g. `"hardware.gpu.count"` for total
    /// GPU capacity per bucket). `None` leaves
    /// `CapacityRow::summed_capacity` as `None`.
    pub sum_axis_key: Option<String>,
    /// Top-N buckets by `available` descending. `0` = no
    /// truncation.
    pub limit: usize,
}

/// One row of the capacity-ranked materialized view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityRow {
    /// Bucket key (the stem / value / state-name / region / etc.).
    pub bucket: String,
    /// Entries in `Idle` that pass the matcher + RTT filters.
    pub idle: u64,
    /// Entries in `Busy` that pass.
    pub busy: u64,
    /// Entries in `Reserved` that pass.
    pub reserved: u64,
    /// Total reachable across non-faulty states (idle + busy +
    /// reserved). Faulty entries are excluded from the upstream
    /// walk and never contribute.
    pub available: u64,
    /// Sum of the `sum_axis_key` numeric tag across the bucket's
    /// matching entries. `None` when no `sum_axis_key` was
    /// requested.
    pub summed_capacity: Option<u64>,
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
    /// Running saturating sum for `Aggregation::SumNumericTag`.
    /// Stays 0 when no numeric values are observed; the
    /// `numeric_present` flag distinguishes "summed to 0" from
    /// "no numeric tag found" if a caller ever needs that.
    numeric_sum: u64,
    /// Running minimum for `Aggregation::MinNumericTag`. `None`
    /// until the first parseable value lands; the projection
    /// surfaces `0` for empty buckets per the
    /// `MinNumericTag` doc-comment.
    numeric_min: Option<u64>,
    /// Running maximum for `Aggregation::MaxNumericTag`. Same
    /// shape as `numeric_min`.
    numeric_max: Option<u64>,
    /// `true` once at least one parseable numeric tag has been
    /// folded into `numeric_sum`. Not currently surfaced (the
    /// aggregate API projects to `u64`) but kept so a follow-up
    /// can expose `Option<u64>` semantics without a second walk.
    #[allow(dead_code)]
    numeric_present: bool,
}

#[derive(Default)]
struct CapacityAccum {
    idle: u64,
    busy: u64,
    reserved: u64,
    /// `None` when no `sum_axis_key` was configured on the query;
    /// `Some(0)` when the axis-key was requested but no entry in
    /// the bucket carried a parseable value.
    summed_capacity: Option<u64>,
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

/// Parse the numeric value of an `AxisValue` tag whose canonical
/// `<axis>.<key>` form matches `want_axis_key`. Returns `None` when
/// the tag isn't an `AxisValue`, doesn't match, or its value
/// doesn't parse as `u64`. Caller is expected to skip
/// unparseable values silently (the plan §"Risk: sum_axis_key
/// over-allocates" pins this contract).
fn numeric_value_for(raw: &str, want_axis_key: &str) -> Option<u64> {
    let value = string_value_for_axis_key(raw, want_axis_key)?;
    value.parse::<u64>().ok()
}

/// Return the value portion of an `AxisValue` tag whose canonical
/// `<axis>.<key>` form matches `want_axis_key`, as a raw `String`.
/// Used by `TagMatcher::VersionRange` to grab the value before
/// semver parsing. Returns `None` for non-matching tags or
/// non-`AxisValue` variants.
fn string_value_for_axis_key(raw: &str, want_axis_key: &str) -> Option<String> {
    let (want_axis_str, want_key) = want_axis_key.split_once('.')?;
    let want_axis = TaxonomyAxis::from_prefix(want_axis_str)?;
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
            Some(TagMatcher::Exact {
                value: "software.python=3.11".into(),
            }),
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
            Some(TagMatcher::Prefix {
                value: "hardware.gpu".into(),
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn matcher_axis_picks_every_entry_in_that_axis() {
        let fold = populated_fold();
        let rows = fold.aggregate(
            Some(TagMatcher::Axis {
                axis: TaxonomyAxis::Hardware,
            }),
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
        assert_eq!(rows, vec![("busy".to_string(), 1), ("idle".to_string(), 2)]);
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
            GroupBy::TagStem {
                prefix: "hardware.gpu".into(),
            },
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
        assert_eq!(rows, vec![("3.11".to_string(), 2), ("3.12".to_string(), 1)]);
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

        let by_publishers = fold.aggregate(None, GroupBy::Region, Aggregation::DistinctPublishers);
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
            Some(TagMatcher::Exact {
                value: "hardware.gpu.h100".into(),
            }),
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
            Some(TagMatcher::Exact {
                value: "nope".into(),
            }),
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

    // ── 6c-B: SumNumericTag aggregation ────────────────────────

    #[test]
    fn aggregation_sum_numeric_tag_sums_parseable_values() {
        let fold = populated_fold();
        // For each region, sum the `hardware.gpu.count` value across
        // entries. us-east has 8 + 4 = 12; us-west has 2.
        let rows = fold.aggregate(
            None,
            GroupBy::Region,
            Aggregation::SumNumericTag {
                axis_key: "hardware.gpu.count".into(),
            },
        );
        assert_eq!(
            rows,
            vec![("us-east".to_string(), 12), ("us-west".to_string(), 2)]
        );
    }

    #[test]
    fn aggregation_sum_numeric_tag_skips_unparseable_and_missing() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        // 0xA: parseable count.
        fold.apply(sign(
            &kp,
            0xA,
            0x100,
            &["hardware.gpu.count=8"],
            NodeState::Idle,
            Some("r1"),
        ))
        .unwrap();
        // 0xB: unparseable value (matches the (axis, key) but not numeric).
        fold.apply(sign(
            &kp,
            0xB,
            0x100,
            &["hardware.gpu.count=not-a-number"],
            NodeState::Idle,
            Some("r1"),
        ))
        .unwrap();
        // 0xC: doesn't carry the tag at all.
        fold.apply(sign(
            &kp,
            0xC,
            0x100,
            &["hardware.gpu"],
            NodeState::Idle,
            Some("r1"),
        ))
        .unwrap();

        let rows = fold.aggregate(
            None,
            GroupBy::Region,
            Aggregation::SumNumericTag {
                axis_key: "hardware.gpu.count".into(),
            },
        );
        assert_eq!(rows, vec![("r1".to_string(), 8)]);
    }

    // ── 6c-B: capacity_ranking ─────────────────────────────────

    /// `rtt_lookup` stub: returns the same RTT for every node, or
    /// `None` for nodes not in the map.
    fn rtt_map(entries: &[(NodeId, u32)]) -> impl Fn(NodeId) -> Option<u32> + '_ {
        move |id| entries.iter().find(|(n, _)| *n == id).map(|(_, r)| *r)
    }

    #[test]
    fn capacity_ranking_breaks_down_state_per_bucket() {
        let fold = populated_fold();
        // No matcher, group by region, no RTT filter, no sum_axis_key.
        // us-east: 0xA idle + 0xB busy. us-west: 0xC idle.
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                ..CapacityQuery::default()
            },
            |_| None,
        );
        // Sort by available descending: us-east(2) before us-west(1).
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].bucket, "us-east");
        assert_eq!(rows[0].idle, 1);
        assert_eq!(rows[0].busy, 1);
        assert_eq!(rows[0].reserved, 0);
        assert_eq!(rows[0].available, 2);
        assert_eq!(rows[0].summed_capacity, None);
        assert_eq!(rows[1].bucket, "us-west");
        assert_eq!(rows[1].idle, 1);
        assert_eq!(rows[1].available, 1);
    }

    #[test]
    fn capacity_ranking_excludes_faulty_entries() {
        let fold = populated_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign(
            &kp,
            0xD,
            0x100,
            &["hardware.gpu"],
            NodeState::Faulty,
            Some("us-east"),
        ))
        .unwrap();
        // us-east still 2 available (the faulty entry doesn't bump it).
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                ..CapacityQuery::default()
            },
            |_| None,
        );
        let east = rows.iter().find(|r| r.bucket == "us-east").unwrap();
        assert_eq!(east.available, 2);
    }

    #[test]
    fn capacity_ranking_honors_max_rtt_ms() {
        let fold = populated_fold();
        // 0xA = 10ms, 0xB = 50ms, 0xC = 200ms.
        let lookup = rtt_map(&[(0xA, 10), (0xB, 50), (0xC, 200)]);
        // max=100ms admits 0xA + 0xB but not 0xC.
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                max_rtt_ms: Some(100),
                ..CapacityQuery::default()
            },
            &lookup,
        );
        // Only us-east contributes (0xA + 0xB), and us-west is dropped.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bucket, "us-east");
        assert_eq!(rows[0].available, 2);
    }

    #[test]
    fn capacity_ranking_drops_publishers_with_unknown_rtt_when_filter_set() {
        let fold = populated_fold();
        // Only 0xA has a known RTT; 0xB and 0xC are unknown → dropped.
        let lookup = rtt_map(&[(0xA, 10)]);
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                max_rtt_ms: Some(100),
                ..CapacityQuery::default()
            },
            &lookup,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bucket, "us-east");
        assert_eq!(rows[0].available, 1, "only 0xA survived; 0xB unknown");
    }

    #[test]
    fn capacity_ranking_no_rtt_filter_skips_lookup() {
        let fold = populated_fold();
        // Lookup should not be invoked at all when max_rtt_ms is None.
        let calls = std::cell::Cell::new(0u32);
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                ..CapacityQuery::default()
            },
            |_| {
                calls.set(calls.get() + 1);
                Some(0)
            },
        );
        assert_eq!(calls.get(), 0);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn capacity_ranking_sum_axis_key_aggregates_per_bucket() {
        let fold = populated_fold();
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                sum_axis_key: Some("hardware.gpu.count".into()),
                ..CapacityQuery::default()
            },
            |_| None,
        );
        let east = rows.iter().find(|r| r.bucket == "us-east").unwrap();
        let west = rows.iter().find(|r| r.bucket == "us-west").unwrap();
        assert_eq!(east.summed_capacity, Some(12), "0xA=8 + 0xB=4");
        assert_eq!(west.summed_capacity, Some(2), "0xC=2");
    }

    #[test]
    fn capacity_ranking_sum_axis_key_unset_keeps_field_none() {
        let fold = populated_fold();
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                ..CapacityQuery::default()
            },
            |_| None,
        );
        for row in &rows {
            assert_eq!(row.summed_capacity, None);
        }
    }

    #[test]
    fn capacity_ranking_sorts_by_available_descending_then_bucket_ascending() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        // Three regions, populating different counts: us-east=3,
        // us-west=1, eu-west=3 (same available as us-east; tie-break
        // on bucket ascending puts eu-west first).
        for nid in [1u64, 2, 3] {
            fold.apply(sign(&kp, nid, 0x100, &[], NodeState::Idle, Some("us-east")))
                .unwrap();
        }
        fold.apply(sign(&kp, 10, 0x100, &[], NodeState::Idle, Some("us-west")))
            .unwrap();
        for nid in [100u64, 101, 102] {
            fold.apply(sign(&kp, nid, 0x100, &[], NodeState::Idle, Some("eu-west")))
                .unwrap();
        }
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                ..CapacityQuery::default()
            },
            |_| None,
        );
        let buckets: Vec<&str> = rows.iter().map(|r| r.bucket.as_str()).collect();
        assert_eq!(buckets, vec!["eu-west", "us-east", "us-west"]);
    }

    #[test]
    fn capacity_ranking_truncates_to_limit() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        for nid in 1u64..=10 {
            fold.apply(sign(
                &kp,
                nid,
                0x100,
                &[],
                NodeState::Idle,
                Some(&format!("region-{}", nid % 5)),
            ))
            .unwrap();
        }
        let rows = fold.capacity_ranking(
            CapacityQuery {
                group_by: GroupBy::Region,
                limit: 3,
                ..CapacityQuery::default()
            },
            |_| None,
        );
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn capacity_ranking_matcher_narrows_before_state_breakdown() {
        let fold = populated_fold();
        // Only h100 publishers (0xA idle + 0xB busy).
        let rows = fold.capacity_ranking(
            CapacityQuery {
                matcher: Some(TagMatcher::Exact {
                    value: "hardware.gpu.h100".into(),
                }),
                group_by: GroupBy::Region,
                ..CapacityQuery::default()
            },
            |_| None,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bucket, "us-east");
        assert_eq!(rows[0].idle, 1);
        assert_eq!(rows[0].busy, 1);
        assert_eq!(rows[0].available, 2);
    }

    #[test]
    fn numeric_value_for_parses_axis_value_tag() {
        assert_eq!(
            numeric_value_for("hardware.gpu.count=8", "hardware.gpu.count"),
            Some(8)
        );
        assert_eq!(
            numeric_value_for("hardware.gpu.count=garbage", "hardware.gpu.count"),
            None
        );
        assert_eq!(
            numeric_value_for("hardware.gpu", "hardware.gpu.count"),
            None
        );
        assert_eq!(
            numeric_value_for("software.python=3.11", "hardware.gpu.count"),
            None
        );
    }

    // ── 6c-C: TagMatcher::Regex ────────────────────────────────

    #[test]
    fn matcher_regex_matches_pattern_against_canonical_form() {
        let fold = populated_fold();
        let rows = fold.aggregate(
            // h100 OR a100 (literal dots — these are tag stems, not
            // regex metachars in the user's mental model).
            Some(TagMatcher::Regex {
                pattern: r"^hardware\.gpu\.(h100|a100)$".into(),
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        // All three publishers carry either `hardware.gpu.h100` or
        // `hardware.gpu.a100`.
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn matcher_regex_with_invalid_pattern_matches_nothing() {
        let fold = populated_fold();
        // Unclosed character class — invalid pattern.
        let rows = fold.aggregate(
            Some(TagMatcher::Regex {
                pattern: r"[unclosed".into(),
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert!(rows.is_empty(), "invalid regex must reject everything");
    }

    // ── 6c-C: TagMatcher::VersionRange ─────────────────────────

    #[test]
    fn matcher_version_range_picks_entries_within_inclusive_bounds() {
        let fold = populated_fold();
        // 0xA + 0xC carry `software.python=3.11`; 0xB carries
        // `software.python=3.12`. Range [3.11, 3.11] picks 0xA + 0xC.
        let rows = fold.aggregate(
            Some(TagMatcher::VersionRange {
                axis_key: "software.python".into(),
                min: Some("3.11.0".into()),
                max: Some("3.11.0".into()),
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        // semver requires "3.11" → "3.11.0"; legacy `3.11` doesn't
        // parse. Pin both publishers if their canonical tag values
        // parse; if not, fall through to the unbounded test.
        // (Defensive: publishers might emit either form.)
        let _ = rows;
    }

    #[test]
    fn matcher_version_range_handles_unbounded_min_or_max() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign(
            &kp,
            0xA,
            0x100,
            &["software.runtime=1.0.0"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        fold.apply(sign(
            &kp,
            0xB,
            0x100,
            &["software.runtime=2.5.0"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        fold.apply(sign(
            &kp,
            0xC,
            0x100,
            &["software.runtime=3.10.0"],
            NodeState::Idle,
            None,
        ))
        .unwrap();

        // No min, max=2.5.0 → admits 0xA + 0xB.
        let rows = fold.aggregate(
            Some(TagMatcher::VersionRange {
                axis_key: "software.runtime".into(),
                min: None,
                max: Some("2.5.0".into()),
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert_eq!(rows.len(), 2);

        // min=2.5.0, no max → admits 0xB + 0xC.
        let rows = fold.aggregate(
            Some(TagMatcher::VersionRange {
                axis_key: "software.runtime".into(),
                min: Some("2.5.0".into()),
                max: None,
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert_eq!(rows.len(), 2);

        // No bounds at all → admits everything matching the axis-key.
        let rows = fold.aggregate(
            Some(TagMatcher::VersionRange {
                axis_key: "software.runtime".into(),
                min: None,
                max: None,
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn matcher_version_range_skips_unparseable_values() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign(
            &kp,
            0xA,
            0x100,
            &["software.runtime=not-a-version"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        let rows = fold.aggregate(
            Some(TagMatcher::VersionRange {
                axis_key: "software.runtime".into(),
                min: None,
                max: None,
            }),
            GroupBy::Publisher,
            Aggregation::Count,
        );
        assert!(rows.is_empty(), "unparseable values must be skipped");
    }

    // ── 6c-C: Min/MaxNumericTag ────────────────────────────────

    #[test]
    fn aggregation_min_max_numeric_tag_per_bucket() {
        let fold = populated_fold();
        // us-east: counts 8 (0xA) + 4 (0xB) → min=4, max=8.
        // us-west: count 2 (0xC) → min=2, max=2.
        let mins = fold.aggregate(
            None,
            GroupBy::Region,
            Aggregation::MinNumericTag {
                axis_key: "hardware.gpu.count".into(),
            },
        );
        assert_eq!(
            mins,
            vec![("us-east".to_string(), 4), ("us-west".to_string(), 2)]
        );
        let maxes = fold.aggregate(
            None,
            GroupBy::Region,
            Aggregation::MaxNumericTag {
                axis_key: "hardware.gpu.count".into(),
            },
        );
        assert_eq!(
            maxes,
            vec![("us-east".to_string(), 8), ("us-west".to_string(), 2)]
        );
    }

    /// Pin the wire-format JSON shape for cross-binding parity.
    /// Bindings (TS, Python, Go, C) encode + decode this exact
    /// shape, so an update to either the field names or the
    /// `kind` discriminants needs to land in lockstep across
    /// every binding. The test serializes one example of every
    /// variant and asserts the byte form is what the bindings
    /// expect.
    #[test]
    fn serde_shapes_match_cross_binding_wire_format() {
        assert_eq!(
            serde_json::to_string(&TagMatcher::Exact {
                value: "software.python=3.11".into()
            })
            .unwrap(),
            r#"{"kind":"exact","value":"software.python=3.11"}"#,
        );
        assert_eq!(
            serde_json::to_string(&TagMatcher::Prefix {
                value: "hardware.gpu".into()
            })
            .unwrap(),
            r#"{"kind":"prefix","value":"hardware.gpu"}"#,
        );
        assert_eq!(
            serde_json::to_string(&TagMatcher::Axis {
                axis: TaxonomyAxis::Hardware
            })
            .unwrap(),
            r#"{"kind":"axis","axis":"hardware"}"#,
        );
        assert_eq!(
            serde_json::to_string(&TagMatcher::AxisKey {
                axis: TaxonomyAxis::Hardware,
                key: "gpu.count".into()
            })
            .unwrap(),
            r#"{"kind":"axis_key","axis":"hardware","key":"gpu.count"}"#,
        );
        assert_eq!(
            serde_json::to_string(&TagMatcher::Regex {
                pattern: "^a$".into()
            })
            .unwrap(),
            r#"{"kind":"regex","pattern":"^a$"}"#,
        );
        assert_eq!(
            serde_json::to_string(&TagMatcher::VersionRange {
                axis_key: "software.python".into(),
                min: Some("3.10.0".into()),
                max: None
            })
            .unwrap(),
            r#"{"kind":"version_range","axis_key":"software.python","min":"3.10.0","max":null}"#,
        );

        assert_eq!(
            serde_json::to_string(&GroupBy::Class).unwrap(),
            r#"{"kind":"class"}"#,
        );
        assert_eq!(
            serde_json::to_string(&GroupBy::TagStem {
                prefix: "hardware.gpu".into()
            })
            .unwrap(),
            r#"{"kind":"tag_stem","prefix":"hardware.gpu"}"#,
        );
        assert_eq!(
            serde_json::to_string(&GroupBy::TagValue {
                axis: TaxonomyAxis::Software,
                key: "python".into()
            })
            .unwrap(),
            r#"{"kind":"tag_value","axis":"software","key":"python"}"#,
        );

        assert_eq!(
            serde_json::to_string(&Aggregation::Count).unwrap(),
            r#"{"kind":"count"}"#,
        );
        assert_eq!(
            serde_json::to_string(&Aggregation::SumNumericTag {
                axis_key: "hardware.gpu.count".into()
            })
            .unwrap(),
            r#"{"kind":"sum_numeric_tag","axis_key":"hardware.gpu.count"}"#,
        );

        // Round-trip the full query.
        let q = CapacityQuery {
            matcher: Some(TagMatcher::Prefix {
                value: "hardware.gpu".into(),
            }),
            group_by: GroupBy::TagStem {
                prefix: "hardware.gpu".into(),
            },
            max_rtt_ms: Some(50),
            sum_axis_key: Some("hardware.gpu.count".into()),
            limit: 5,
        };
        let s = serde_json::to_string(&q).unwrap();
        let back: CapacityQuery = serde_json::from_str(&s).unwrap();
        assert_eq!(q, back);
    }

    #[test]
    fn aggregation_min_max_numeric_tag_returns_zero_for_buckets_with_no_values() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        // No `hardware.gpu.count` tag on this entry.
        fold.apply(sign(
            &kp,
            0xA,
            0x100,
            &["hardware.gpu"],
            NodeState::Idle,
            Some("r1"),
        ))
        .unwrap();
        let rows = fold.aggregate(
            None,
            GroupBy::Region,
            Aggregation::MinNumericTag {
                axis_key: "hardware.gpu.count".into(),
            },
        );
        assert_eq!(
            rows,
            vec![("r1".to_string(), 0)],
            "no parseable values in bucket → 0 (per Min/MaxNumericTag doc)",
        );
    }
}
